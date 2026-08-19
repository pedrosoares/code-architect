//! Settings popup: add, edit, and remove saved API configurations and MCP
//! servers.
//!
//! Opened from the gear button in [`crate::shell::Header`]. Two independent
//! sections share one popup, switched by [`Section`] — API configurations
//! ("profiles": a name, provider kind, optional base URL and key, and a
//! model — everything `architect_llm::ProviderConfig` needs, plus a label;
//! activating one rebuilds the running agent in place, the conversation
//! continues, only which API answers the next turn changes) and MCP servers
//! (a name, a command to spawn, and its args/env — every enabled one's tools
//! join the same tool set `read_file`/`write_file`/etc. already sit in,
//! reconnected whenever the saved list changes).

use architect_config::{DocsConfig, IntegrationsConfig, McpServerConfig, McpTransport, Profile};
use architect_ui::theme;
use freya::components::{
    Button, Input, InputMode, MenuItem, Popup, PopupButtons, PopupContent, PopupTitle, ScrollView,
    Select,
};
use freya::prelude::*;
use uuid::Uuid;

use crate::{engine::Engine, oauth::OAuthProvider, state::Transcript};

#[derive(Debug, Clone, Copy, PartialEq)]
enum Section {
    Profiles,
    LmStudio,
    McpServers,
    Integrations,
    Documentation,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    List,
    Add,
    Edit(Uuid),
}

/// The text fields of the profile add/edit form. Each one is its own
/// `State<String>` rather than a single `State<Draft>` — `Input` binds
/// directly to a `Writable<String>`, so there is nothing to gain from
/// bundling them, only a destructuring step at every read.
#[derive(Clone, Copy)]
struct Fields {
    name: State<String>,
    kind: State<String>,
    base_url: State<String>,
    api_key: State<String>,
    model: State<String>,
}

impl Fields {
    fn clear(&mut self) {
        self.name.set(String::new());
        self.kind.set("openai".to_owned());
        self.base_url.set(String::new());
        self.api_key.set(String::new());
        self.model.set(String::new());
    }

    fn load(&mut self, profile: &Profile) {
        self.name.set(profile.name.clone());
        self.kind.set(profile.kind.clone());
        self.base_url
            .set(profile.base_url.clone().unwrap_or_default());
        self.api_key
            .set(profile.api_key.clone().unwrap_or_default());
        self.model.set(profile.model.clone());
    }

    fn to_profile(self, id: Uuid) -> Profile {
        Profile {
            id,
            name: self.name.read().clone(),
            kind: self.kind.read().clone(),
            base_url: non_empty(self.base_url.read().clone()),
            api_key: non_empty(self.api_key.read().clone()),
            model: self.model.read().clone(),
        }
    }
}

/// Which set of fields the MCP server form shows — mirrors
/// `McpTransport`'s two variants, but as plain UI state rather than the
/// config type itself, the same way `Fields::kind` is a bare `String`
/// rather than `architect_llm::ProviderConfig`'s own kind type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpTransportKind {
    Stdio,
    Http,
}

/// The text fields of the MCP server add/edit form. `args` and `env` are
/// each one `Input`'s worth of text — a space-separated command line and
/// `KEY=VALUE` lines respectively — parsed back into `Vec`/`HashMap` only on
/// save, the same "one `State<String>` per field" reasoning as [`Fields`].
/// `url`/`bearer_token` are the `Http` transport's equivalent; only one
/// transport's fields are shown at a time, per `transport`, but both stay
/// populated so switching back and forth doesn't lose what was typed.
#[derive(Clone, Copy)]
struct McpFields {
    name: State<String>,
    transport: State<McpTransportKind>,
    command: State<String>,
    args: State<String>,
    env: State<String>,
    url: State<String>,
    bearer_token: State<String>,
}

impl McpFields {
    fn clear(&mut self) {
        self.name.set(String::new());
        self.transport.set(McpTransportKind::Stdio);
        self.command.set(String::new());
        self.args.set(String::new());
        self.env.set(String::new());
        self.url.set(String::new());
        self.bearer_token.set(String::new());
    }

    fn load(&mut self, server: &McpServerConfig) {
        self.name.set(server.name.clone());
        match &server.transport {
            McpTransport::Stdio { command, args, env } => {
                self.transport.set(McpTransportKind::Stdio);
                self.command.set(command.clone());
                self.args.set(args.join(" "));
                self.env.set(
                    env.iter()
                        .map(|(key, value)| format!("{key}={value}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            }
            McpTransport::Http { url, bearer_token } => {
                self.transport.set(McpTransportKind::Http);
                self.url.set(url.clone());
                self.bearer_token
                    .set(bearer_token.clone().unwrap_or_default());
            }
        }
    }

    fn to_server(self, id: Uuid, enabled: bool) -> McpServerConfig {
        let transport = match *self.transport.read() {
            McpTransportKind::Stdio => {
                let args = self
                    .args
                    .read()
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect();
                let env = self
                    .env
                    .read()
                    .lines()
                    .filter_map(|line| line.split_once('='))
                    .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
                    .collect();
                McpTransport::Stdio {
                    command: self.command.read().clone(),
                    args,
                    env,
                }
            }
            McpTransportKind::Http => McpTransport::Http {
                url: self.url.read().clone(),
                bearer_token: non_empty(self.bearer_token.read().clone()),
            },
        };

        McpServerConfig {
            id,
            name: self.name.read().clone(),
            enabled,
            transport,
        }
    }
}

fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// The three credential fields of the Integrations form. No `Mode` (unlike
/// `Fields`/`McpFields`) — there is no list to browse and no separate
/// add/edit flow, just one persistent set of values that gets loaded when
/// the tab is opened and saved back wholesale. `*_pending` tracks a login
/// in flight for that provider — set by its "Login" button, cleared by the
/// side effect in `SettingsPanel::render` once the resulting
/// `IntegrationsListed` arrives.
#[derive(Clone, Copy)]
struct IntegrationsFields {
    github_token: State<String>,
    github_use_gh_cli: State<bool>,
    slack_token: State<String>,
    linear_api_key: State<String>,
    github_pending: State<bool>,
    slack_pending: State<bool>,
    linear_pending: State<bool>,
}

impl IntegrationsFields {
    fn load(&mut self, integrations: &IntegrationsConfig) {
        self.github_token
            .set(integrations.github_token.clone().unwrap_or_default());
        self.github_use_gh_cli.set(integrations.github_use_gh_cli);
        self.slack_token
            .set(integrations.slack_token.clone().unwrap_or_default());
        self.linear_api_key
            .set(integrations.linear_api_key.clone().unwrap_or_default());
    }

    fn to_config(self) -> IntegrationsConfig {
        let use_gh_cli = *self.github_use_gh_cli.read();
        IntegrationsConfig {
            // A stale pasted token should never linger behind an active
            // gh-CLI mode — turning the toggle back off just means
            // pasting a token again.
            github_token: if use_gh_cli {
                None
            } else {
                non_empty(self.github_token.read().clone())
            },
            github_use_gh_cli: use_gh_cli,
            slack_token: non_empty(self.slack_token.read().clone()),
            linear_api_key: non_empty(self.linear_api_key.read().clone()),
        }
    }
}

/// The Documentation tab's fields — same "loaded on tab select, saved
/// wholesale" shape as [`IntegrationsFields`], just two fields instead of
/// several credentials. `driver` isn't a field here yet: v1 only has
/// Obsidian, so [`DocsFields::to_config`] fixes it rather than offering a
/// choice with nothing else to pick.
#[derive(Clone, Copy)]
struct DocsFields {
    enabled: State<bool>,
    vault_path: State<String>,
}

impl DocsFields {
    fn load(&mut self, docs: &DocsConfig) {
        self.enabled.set(docs.enabled);
        self.vault_path
            .set(docs.vault_path.clone().unwrap_or_default());
    }

    fn to_config(self) -> DocsConfig {
        DocsConfig {
            enabled: *self.enabled.read(),
            driver: "obsidian".to_owned(),
            vault_path: non_empty(self.vault_path.read().clone()),
        }
    }
}

/// Shown next to the gear icon; toggling it is all [`crate::shell::Header`]
/// needs to know about this component.
#[derive(PartialEq)]
pub struct SettingsPanel {
    pub show: State<bool>,
}

impl Component for SettingsPanel {
    fn render(&self) -> impl IntoElement {
        let mut show = self.show;
        let engine = consume_context::<Engine>();
        let transcript = consume_context::<State<Transcript>>();

        let (
            profiles,
            active,
            mcp_servers,
            integrations,
            docs,
            discovered_models,
            active_adhoc_model,
        ) = {
            let transcript = transcript.read();
            (
                transcript.profiles.clone(),
                transcript.active_profile,
                transcript.mcp_servers.clone(),
                transcript.integrations.clone(),
                transcript.docs.clone(),
                transcript.discovered_models.clone(),
                transcript.active_adhoc_model.clone(),
            )
        };

        let section = use_state(|| Section::Profiles);
        let mut mode = use_state(|| Mode::List);
        let fields = Fields {
            name: use_state(String::new),
            kind: use_state(|| "openai".to_owned()),
            base_url: use_state(String::new),
            api_key: use_state(String::new),
            model: use_state(String::new),
        };
        // Not saved anywhere — what a local server has loaded changes over
        // time, so there is nothing meaningful to persist here (the whole
        // reason this is its own page rather than folded into `Fields`
        // above). Living in `use_state` the same way `fields` does means
        // the typed base URL survives closing and reopening the popup for
        // as long as the app keeps running, without ever touching disk.
        let lmstudio_fields = LmStudioFields {
            base_url: use_state(String::new),
            api_key: use_state(String::new),
        };
        let mut mcp_mode = use_state(|| Mode::List);
        let mcp_fields = McpFields {
            name: use_state(String::new),
            transport: use_state(|| McpTransportKind::Stdio),
            command: use_state(String::new),
            args: use_state(String::new),
            env: use_state(String::new),
            url: use_state(String::new),
            bearer_token: use_state(String::new),
        };
        let mut integrations_fields = IntegrationsFields {
            github_token: use_state(String::new),
            github_use_gh_cli: use_state(|| false),
            slack_token: use_state(String::new),
            linear_api_key: use_state(String::new),
            github_pending: use_state(|| false),
            slack_pending: use_state(|| false),
            linear_pending: use_state(|| false),
        };
        let docs_fields = DocsFields {
            enabled: use_state(|| true),
            vault_path: use_state(String::new),
        };

        // The one reactive-sync path in this file — every other field here
        // is deliberately load-on-tab-select only (an unrelated event could
        // otherwise clobber an in-progress manual edit), but this is scoped
        // to exactly `integrations` changing while a login this button
        // started is still pending, so it only ever fires right after a
        // completed (or failed) OAuth login for this exact panel instance.
        use_side_effect(move || {
            let integrations = transcript.read().integrations.clone();
            if *integrations_fields.github_pending.read() {
                integrations_fields
                    .github_token
                    .set(integrations.github_token.clone().unwrap_or_default());
                integrations_fields.github_pending.set(false);
            }
            if *integrations_fields.slack_pending.read() {
                integrations_fields
                    .slack_token
                    .set(integrations.slack_token.clone().unwrap_or_default());
                integrations_fields.slack_pending.set(false);
            }
            if *integrations_fields.linear_pending.read() {
                integrations_fields
                    .linear_api_key
                    .set(integrations.linear_api_key.clone().unwrap_or_default());
                integrations_fields.linear_pending.set(false);
            }
        });

        Popup::new()
            // Wide enough for all four section tabs on one line — at the
            // old 480px, adding "LM Studio" pushed "Integrations" onto two
            // lines.
            .width(Size::px(560.0))
            .on_close_request(move |_| {
                show.set(false);
                mode.set(Mode::List);
                mcp_mode.set(Mode::List);
            })
            .maybe(*show.read(), move |popup| {
                // The section tabs only make sense while browsing a list —
                // mid add/edit, switching sections out from under an
                // unsaved form would be confusing.
                let browsing = *mode.read() == Mode::List && *mcp_mode.read() == Mode::List;
                let popup = if browsing {
                    popup.child(section_tabs(
                        section,
                        integrations.clone(),
                        integrations_fields,
                        docs.clone(),
                        docs_fields,
                    ))
                } else {
                    popup
                };

                match *section.read() {
                    Section::Profiles => profiles_popup(
                        popup,
                        *mode.read(),
                        profiles.clone(),
                        active,
                        engine.clone(),
                        mode,
                        fields,
                        show,
                    ),
                    Section::LmStudio => lmstudio_popup(
                        popup,
                        engine.clone(),
                        discovered_models.clone(),
                        active_adhoc_model.clone(),
                        lmstudio_fields,
                        show,
                    ),
                    Section::McpServers => mcp_popup(
                        popup,
                        *mcp_mode.read(),
                        mcp_servers.clone(),
                        engine.clone(),
                        mcp_mode,
                        mcp_fields,
                        show,
                    ),
                    Section::Integrations => integrations_popup(
                        popup,
                        engine.clone(),
                        integrations.clone(),
                        integrations_fields,
                        show,
                    ),
                    Section::Documentation => docs_popup(popup, engine.clone(), docs_fields, show),
                }
            })
    }
}

fn section_tabs(
    mut section: State<Section>,
    integrations: IntegrationsConfig,
    mut integrations_fields: IntegrationsFields,
    docs: DocsConfig,
    mut docs_fields: DocsFields,
) -> impl IntoElement {
    let current = *section.read();

    let tab = |label: &'static str, this: Section, mut section: State<Section>| {
        let button = Button::new().on_press(move |_| section.set(this));
        if current == this {
            button.filled()
        } else {
            button.outline()
        }
        .child(label)
    };

    let integrations_tab = {
        let button = Button::new().on_press(move |_| {
            // Loaded on select, not preloaded at mount — the same reason
            // `mcp_row`'s Edit button calls `fields.load(&server)` rather
            // than the fields starting pre-filled: this component mounts
            // once, long before the real saved values have necessarily
            // arrived from the engine.
            integrations_fields.load(&integrations);
            // Also the escape hatch if a login's `pending` flag ever gets
            // stuck (e.g. the browser flow failed, which reports a global
            // `Failed` rather than a change to `integrations` the pending-
            // clearing side effect in `SettingsPanel::render` watches for)
            // — reselecting this tab always leaves every button re-clickable.
            integrations_fields.github_pending.set(false);
            integrations_fields.slack_pending.set(false);
            integrations_fields.linear_pending.set(false);
            section.set(Section::Integrations);
        });
        if current == Section::Integrations {
            button.filled()
        } else {
            button.outline()
        }
        .child("Integrations")
    };

    let docs_tab = {
        let button = Button::new().on_press(move |_| {
            // Loaded on select, not preloaded at mount — same reason
            // `integrations_tab` above does this.
            docs_fields.load(&docs);
            section.set(Section::Documentation);
        });
        if current == Section::Documentation {
            button.filled()
        } else {
            button.outline()
        }
        .child("Documentation")
    };

    rect()
        .width(Size::fill())
        .horizontal()
        .spacing(theme::SPACE_SM)
        .padding(Gaps::new(8.0, 8.0, 0.0, 8.0))
        .child(tab("API configurations", Section::Profiles, section))
        .child(tab("LM Studio", Section::LmStudio, section))
        .child(tab("MCP servers", Section::McpServers, section))
        .child(integrations_tab)
        .child(docs_tab)
}

// Every argument here is a distinct piece of render-time state this single
// call site already has in hand — bundling them into a struct would only
// add a level of indirection for the one caller that builds it.
#[allow(clippy::too_many_arguments)]
fn profiles_popup(
    popup: Popup,
    mode: Mode,
    profiles: Vec<Profile>,
    active: Option<Uuid>,
    engine: Engine,
    mut mode_state: State<Mode>,
    fields: Fields,
    mut show: State<bool>,
) -> Popup {
    match mode {
        Mode::List => popup
            .child(PopupTitle::new("API configurations".to_owned()))
            .child(PopupContent::new().child(profile_list(
                profiles,
                active,
                engine.clone(),
                mode_state,
                fields,
                engine.config().kind.clone(),
                engine.config().label().to_owned(),
            )))
            .child(
                PopupButtons::new()
                    .child(
                        Button::new()
                            .outline()
                            .on_press(move |_| show.set(false))
                            .child("Close"),
                    )
                    .child(
                        Button::new()
                            .filled()
                            .on_press(move |_| {
                                let mut fields = fields;
                                fields.clear();
                                mode_state.set(Mode::Add);
                            })
                            .child("+ Add"),
                    ),
            ),
        editing => {
            let is_edit = matches!(editing, Mode::Edit(_));
            popup
                .child(PopupTitle::new(
                    if is_edit {
                        "Edit configuration"
                    } else {
                        "Add configuration"
                    }
                    .to_owned(),
                ))
                .child(PopupContent::new().child(form(fields)))
                .child(
                    PopupButtons::new()
                        .child(
                            Button::new()
                                .outline()
                                .on_press(move |_| mode_state.set(Mode::List))
                                .child("Cancel"),
                        )
                        .child(
                            Button::new()
                                .filled()
                                .on_press(move |_| {
                                    let id = match editing {
                                        Mode::Edit(id) => id,
                                        _ => Uuid::new_v4(),
                                    };
                                    engine.save_profile(fields.to_profile(id));
                                    mode_state.set(Mode::List);
                                })
                                .child("Save"),
                        ),
                )
        }
    }
}

fn profile_list(
    profiles: Vec<Profile>,
    active: Option<Uuid>,
    engine: Engine,
    mode: State<Mode>,
    fields: Fields,
    default_kind: String,
    default_model: String,
) -> impl IntoElement {
    let mut list = rect()
        .width(Size::fill())
        .vertical()
        .spacing(theme::SPACE_SM);

    // Always present — this is what "Use" reverts to below, and the only
    // thing shown active until a saved configuration is activated.
    list = list.child(default_row(
        active.is_none(),
        engine.clone(),
        default_kind,
        default_model,
    ));

    if profiles.is_empty() {
        list = list.child(
            rect()
                .width(Size::fill())
                .padding(Gaps::new_all(theme::SPACE_MD))
                .child(
                    label()
                        .text(
                            "No saved configurations yet. Add one to talk to a different \
                             provider or model.",
                        )
                        .color(theme::TEXT_DIM)
                        .font_size(theme::FONT_SMALL),
                ),
        );
    } else {
        for profile in profiles {
            let is_active = Some(profile.id) == active;
            list = list.child(profile_row(
                profile,
                is_active,
                engine.clone(),
                mode,
                fields,
            ));
        }
    }

    list
}

/// The configuration this engine started with (env vars, or the built-in
/// local-server default) — always available to revert to, even before any
/// profile is ever saved.
fn default_row(is_active: bool, engine: Engine, kind: String, model: String) -> impl IntoElement {
    let (background, name_color) = if is_active {
        (theme::ACCENT_SOFT, theme::TEXT)
    } else {
        (theme::SURFACE, theme::TEXT_DIM)
    };

    rect()
        .width(Size::fill())
        .horizontal()
        .content(Content::Flex)
        .cross_align(Alignment::Center)
        .spacing(theme::SPACE_SM)
        .padding(Gaps::new_symmetric(10.0, theme::SPACE_MD))
        .background(background)
        .rounded_lg()
        .child(
            rect()
                .width(Size::flex(1.0))
                .vertical()
                .spacing(3.0)
                .child(
                    label()
                        .text("Default")
                        .color(name_color)
                        .max_lines(1)
                        .text_overflow(TextOverflow::Ellipsis),
                )
                .child(
                    label()
                        .text(format!("{} · {}", kind_label(&kind), model))
                        .color(theme::TEXT_DIM)
                        .font_size(theme::FONT_SMALL)
                        .max_lines(1)
                        .text_overflow(TextOverflow::Ellipsis),
                ),
        )
        .child(if is_active {
            label()
                .text("Active")
                .color(theme::ACCENT)
                .font_size(theme::FONT_SMALL)
                .into_element()
        } else {
            Button::new()
                .outline()
                .on_press(move |_| engine.deactivate_profile())
                .child("Use")
                .into_element()
        })
}

fn profile_row(
    profile: Profile,
    is_active: bool,
    engine: Engine,
    mut mode: State<Mode>,
    mut fields: Fields,
) -> impl IntoElement {
    let id = profile.id;
    let (background, name_color) = if is_active {
        (theme::ACCENT_SOFT, theme::TEXT)
    } else {
        (theme::SURFACE, theme::TEXT_DIM)
    };

    rect()
        .width(Size::fill())
        .horizontal()
        // The name/model column shares the row with three buttons — without
        // flex content, `Size::flex(1.0)` below claims everything and
        // squeezes the buttons down to single-character-wide columns.
        .content(Content::Flex)
        .cross_align(Alignment::Center)
        .spacing(theme::SPACE_SM)
        .padding(Gaps::new_symmetric(10.0, theme::SPACE_MD))
        .background(background)
        .rounded_lg()
        .child(
            rect()
                .width(Size::flex(1.0))
                .vertical()
                .spacing(3.0)
                .child(
                    label()
                        .text(profile.name.clone())
                        .color(name_color)
                        .max_lines(1)
                        .text_overflow(TextOverflow::Ellipsis),
                )
                .child(
                    label()
                        .text(format!("{} · {}", kind_label(&profile.kind), profile.model))
                        .color(theme::TEXT_DIM)
                        .font_size(theme::FONT_SMALL)
                        .max_lines(1)
                        .text_overflow(TextOverflow::Ellipsis),
                ),
        )
        .child(if is_active {
            label()
                .text("Active")
                .color(theme::ACCENT)
                .font_size(theme::FONT_SMALL)
                .into_element()
        } else {
            let engine = engine.clone();
            Button::new()
                .outline()
                .on_press(move |_| engine.activate_profile(id))
                .child("Use")
                .into_element()
        })
        .child({
            let profile = profile.clone();
            Button::new()
                .outline()
                .on_press(move |_| {
                    fields.load(&profile);
                    mode.set(Mode::Edit(id));
                })
                .child("Edit")
        })
        .child(
            Button::new()
                .outline()
                .on_press(move |_| engine.delete_profile(id))
                .child("Delete"),
        )
}

/// The Base URL/API key inputs on the LM Studio page — deliberately not
/// part of `Fields`/`Profile`: nothing here is ever saved (see
/// `lmstudio_popup`'s docs), this only exists to remember what to fetch
/// against for as long as the app keeps running.
#[derive(Clone, Copy)]
struct LmStudioFields {
    base_url: State<String>,
    api_key: State<String>,
}

/// A live view of an OpenAI-compatible local server's currently loaded
/// models — LM Studio, vLLM, Ollama, and the rest of the family this app
/// already treats as "just a different base URL." Deliberately its own
/// page, not folded into "API configurations": what a server has loaded
/// changes over time, so persisting a snapshot of it as a saved `Profile`
/// would only ever be stale the moment it's written. Picking a model here
/// activates it for live turns immediately (`Engine::use_ad_hoc_model`)
/// without saving anything.
fn lmstudio_popup(
    popup: Popup,
    engine: Engine,
    discovered_models: Vec<String>,
    active_adhoc_model: Option<String>,
    fields: LmStudioFields,
    mut show: State<bool>,
) -> Popup {
    let fetch_engine = engine.clone();

    popup
        .child(PopupTitle::new("LM Studio".to_owned()))
        .child(
            PopupContent::new().child(
                rect()
                    .width(Size::fill())
                    .vertical()
                    .spacing(theme::SPACE_SM)
                    .child(field_label("Base URL"))
                    .child(
                        Input::new(fields.base_url)
                            .placeholder("http://localhost:1234/v1")
                            .width(Size::fill()),
                    )
                    .child(field_label("API key (optional)"))
                    .child(
                        Input::new(fields.api_key)
                            .mode(InputMode::new_password())
                            .placeholder("sk-...")
                            .width(Size::fill()),
                    )
                    .child(
                        Button::new()
                            .outline()
                            .on_press(move |_| {
                                let base_url = non_empty(fields.base_url.read().clone())
                                    .unwrap_or_else(|| "http://localhost:1234/v1".to_owned());
                                // Write the resolved URL back into the
                                // field — otherwise it stays empty even
                                // after a successful fetch against the
                                // assumed default, and "Use" below would
                                // then activate a model against whatever
                                // is left in the (empty) field.
                                let mut base_url_field = fields.base_url;
                                base_url_field.set(base_url.clone());
                                fetch_engine.list_models(
                                    base_url,
                                    non_empty(fields.api_key.read().clone()),
                                );
                            })
                            .child("Fetch models"),
                    )
                    .child(if discovered_models.is_empty() {
                        label()
                            .text("No models fetched yet.")
                            .color(theme::TEXT_DIM)
                            .font_size(theme::FONT_SMALL)
                            .into_element()
                    } else {
                        // A fixed height + `ScrollView` rather than letting
                        // the popup grow — a server with a couple dozen
                        // models loaded must not push "Close" off-screen.
                        rect()
                            .width(Size::fill())
                            .height(Size::px(280.0))
                            .child(ScrollView::new().child({
                                let mut list = rect()
                                    .width(Size::fill())
                                    .vertical()
                                    .spacing(theme::SPACE_SM);
                                for model in discovered_models {
                                    let is_active =
                                        active_adhoc_model.as_deref() == Some(model.as_str());
                                    list = list.child(lmstudio_model_row(
                                        model,
                                        is_active,
                                        engine.clone(),
                                        fields,
                                    ));
                                }
                                list
                            }))
                            .into_element()
                    }),
            ),
        )
        .child(
            PopupButtons::new().child(
                Button::new()
                    .outline()
                    .on_press(move |_| show.set(false))
                    .child("Close"),
            ),
        )
}

fn lmstudio_model_row(
    model: String,
    is_active: bool,
    engine: Engine,
    fields: LmStudioFields,
) -> impl IntoElement {
    let (background, name_color) = if is_active {
        (theme::ACCENT_SOFT, theme::TEXT)
    } else {
        (theme::SURFACE, theme::TEXT_DIM)
    };

    rect()
        .width(Size::fill())
        .horizontal()
        .content(Content::Flex)
        .cross_align(Alignment::Center)
        .spacing(theme::SPACE_SM)
        .padding(Gaps::new_symmetric(10.0, theme::SPACE_MD))
        .background(background)
        .rounded_lg()
        .child(
            label()
                .width(Size::flex(1.0))
                .text(model.clone())
                .color(name_color)
                .max_lines(1)
                .text_overflow(TextOverflow::Ellipsis),
        )
        .child(if is_active {
            label()
                .text("Active")
                .color(theme::ACCENT)
                .font_size(theme::FONT_SMALL)
                .into_element()
        } else {
            Button::new()
                .outline()
                .on_press(move |_| {
                    // Resolved the same way "Fetch models" resolves it —
                    // a model can only be picked from a list that was
                    // fetched against a real base URL already, but this
                    // keeps `Command::UseAdHocModel`'s `base_url` a
                    // required, never-empty `String` by construction
                    // rather than relying on the field still holding
                    // what was actually queried.
                    let base_url = non_empty(fields.base_url.read().clone())
                        .unwrap_or_else(|| "http://localhost:1234/v1".to_owned());
                    engine.use_ad_hoc_model(
                        base_url,
                        non_empty(fields.api_key.read().clone()),
                        model.clone(),
                    );
                })
                .child("Use")
                .into_element()
        })
}

fn form(fields: Fields) -> impl IntoElement {
    rect()
        .width(Size::fill())
        .vertical()
        .spacing(theme::SPACE_SM)
        .child(field_label("Name"))
        .child(
            Input::new(fields.name)
                .placeholder("e.g. Work OpenRouter")
                .width(Size::fill()),
        )
        .child(field_label("Provider"))
        .child(kind_select(fields.kind))
        .child(field_label(
            "Base URL (optional — leave empty for the provider's default)",
        ))
        .child(
            Input::new(fields.base_url)
                .placeholder("http://localhost:1234/v1")
                .width(Size::fill()),
        )
        .child(field_label("API key (optional)"))
        .child(
            Input::new(fields.api_key)
                .mode(InputMode::new_password())
                .placeholder("sk-...")
                .width(Size::fill()),
        )
        .child(field_label("Model"))
        .child(
            Input::new(fields.model)
                .placeholder("qwen/qwen3.8-27b")
                .width(Size::fill()),
        )
}

fn kind_select(mut kind: State<String>) -> impl IntoElement {
    let current = kind.read().clone();

    Select::new()
        .selected_item(kind_label(&current).to_owned())
        .children(["openai", "anthropic"].into_iter().map(|value| {
            MenuItem::new()
                .selected(current == value)
                .on_press(move |_| kind.set(value.to_owned()))
                .child(kind_label(value).to_owned())
                .into()
        }))
}

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "anthropic" => "Anthropic",
        _ => "OpenAI-compatible",
    }
}

#[allow(clippy::too_many_arguments)]
fn mcp_popup(
    popup: Popup,
    mode: Mode,
    servers: Vec<McpServerConfig>,
    engine: Engine,
    mut mode_state: State<Mode>,
    fields: McpFields,
    mut show: State<bool>,
) -> Popup {
    match mode {
        Mode::List => popup
            .child(PopupTitle::new("MCP servers".to_owned()))
            .child(PopupContent::new().child(mcp_list(servers, engine.clone(), mode_state, fields)))
            .child(
                PopupButtons::new()
                    .child(
                        Button::new()
                            .outline()
                            .on_press(move |_| show.set(false))
                            .child("Close"),
                    )
                    .child(
                        Button::new()
                            .filled()
                            .on_press(move |_| {
                                let mut fields = fields;
                                fields.clear();
                                mode_state.set(Mode::Add);
                            })
                            .child("+ Add"),
                    ),
            ),
        editing => {
            let is_edit = matches!(editing, Mode::Edit(_));
            popup
                .child(PopupTitle::new(
                    if is_edit {
                        "Edit MCP server"
                    } else {
                        "Add MCP server"
                    }
                    .to_owned(),
                ))
                .child(PopupContent::new().child(mcp_form(fields)))
                .child(
                    PopupButtons::new()
                        .child(
                            Button::new()
                                .outline()
                                .on_press(move |_| mode_state.set(Mode::List))
                                .child("Cancel"),
                        )
                        .child(
                            Button::new()
                                .filled()
                                .on_press(move |_| {
                                    let id = match editing {
                                        Mode::Edit(id) => id,
                                        _ => Uuid::new_v4(),
                                    };
                                    engine.save_mcp_server(fields.to_server(id, true));
                                    mode_state.set(Mode::List);
                                })
                                .child("Save"),
                        ),
                )
        }
    }
}

fn mcp_list(
    servers: Vec<McpServerConfig>,
    engine: Engine,
    mode: State<Mode>,
    fields: McpFields,
) -> impl IntoElement {
    if servers.is_empty() {
        return rect()
            .width(Size::fill())
            .padding(Gaps::new_all(theme::SPACE_MD))
            .child(
                label()
                    .text(
                        "No MCP servers saved yet. Add one to give the agent extra tools \
                         from outside this app.",
                    )
                    .color(theme::TEXT_DIM)
                    .font_size(theme::FONT_SMALL),
            )
            .into_element();
    }

    let mut list = rect()
        .width(Size::fill())
        .vertical()
        .spacing(theme::SPACE_SM);

    for server in servers {
        list = list.child(mcp_row(server, engine.clone(), mode, fields));
    }

    list.into_element()
}

fn mcp_row(
    server: McpServerConfig,
    engine: Engine,
    mut mode: State<Mode>,
    mut fields: McpFields,
) -> impl IntoElement {
    let id = server.id;
    let (background, name_color) = if server.enabled {
        (theme::SURFACE, theme::TEXT)
    } else {
        (theme::SURFACE, theme::TEXT_DIM)
    };

    let command_line = match &server.transport {
        McpTransport::Stdio { command, args, .. } if args.is_empty() => command.clone(),
        McpTransport::Stdio { command, args, .. } => format!("{command} {}", args.join(" ")),
        McpTransport::Http { url, .. } => url.clone(),
    };

    rect()
        .width(Size::fill())
        .horizontal()
        // Same reasoning as `profile_row`: without flex content the name
        // column claims the whole row and squeezes the buttons down to
        // single-character-wide columns.
        .content(Content::Flex)
        .cross_align(Alignment::Center)
        .spacing(theme::SPACE_SM)
        .padding(Gaps::new_symmetric(10.0, theme::SPACE_MD))
        .background(background)
        .rounded_lg()
        .child(
            rect()
                .width(Size::flex(1.0))
                .vertical()
                .spacing(3.0)
                .child(
                    label()
                        .text(server.name.clone())
                        .color(name_color)
                        .max_lines(1)
                        .text_overflow(TextOverflow::Ellipsis),
                )
                .child(
                    label()
                        .text(command_line)
                        .color(theme::TEXT_DIM)
                        .font_size(theme::FONT_SMALL)
                        .font_family(theme::FONT_MONO)
                        .max_lines(1)
                        .text_overflow(TextOverflow::Ellipsis),
                ),
        )
        .child({
            let engine = engine.clone();
            let current = server.clone();
            Button::new()
                .outline()
                .on_press(move |_| {
                    let toggled = McpServerConfig {
                        enabled: !current.enabled,
                        ..current.clone()
                    };
                    engine.save_mcp_server(toggled);
                })
                .child(if server.enabled {
                    "Enabled"
                } else {
                    "Disabled"
                })
        })
        .child({
            let server = server.clone();
            Button::new()
                .outline()
                .on_press(move |_| {
                    fields.load(&server);
                    mode.set(Mode::Edit(id));
                })
                .child("Edit")
        })
        .child(
            Button::new()
                .outline()
                .on_press(move |_| engine.delete_mcp_server(id))
                .child("Delete"),
        )
}

fn mcp_form(fields: McpFields) -> impl IntoElement {
    let transport = *fields.transport.read();

    rect()
        .width(Size::fill())
        .vertical()
        .spacing(theme::SPACE_SM)
        .child(field_label("Name"))
        .child(
            Input::new(fields.name)
                .placeholder("e.g. Git")
                .width(Size::fill()),
        )
        .child(field_label("Transport"))
        .child(mcp_transport_select(fields.transport))
        .maybe_child((transport == McpTransportKind::Stdio).then(|| stdio_fields(fields)))
        .maybe_child((transport == McpTransportKind::Http).then(|| http_fields(fields)))
}

fn stdio_fields(fields: McpFields) -> impl IntoElement {
    rect()
        .width(Size::fill())
        .vertical()
        .spacing(theme::SPACE_SM)
        .child(field_label("Command"))
        .child(
            Input::new(fields.command)
                .placeholder("uvx, npx, ...")
                .width(Size::fill()),
        )
        .child(field_label("Arguments (space-separated)"))
        .child(
            Input::new(fields.args)
                .placeholder("mcp-server-git")
                .width(Size::fill()),
        )
        .child(field_label(
            "Environment (optional — one KEY=VALUE per line)",
        ))
        .child(
            Input::new(fields.env)
                .placeholder("GITHUB_TOKEN=ghp_...")
                .width(Size::fill()),
        )
}

fn http_fields(fields: McpFields) -> impl IntoElement {
    rect()
        .width(Size::fill())
        .vertical()
        .spacing(theme::SPACE_SM)
        .child(field_label("URL"))
        .child(
            Input::new(fields.url)
                .placeholder("https://example.com/mcp")
                .width(Size::fill()),
        )
        .child(field_label("Bearer token (optional)"))
        .child(
            Input::new(fields.bearer_token)
                .mode(InputMode::new_password())
                .placeholder("...")
                .width(Size::fill()),
        )
}

fn mcp_transport_select(mut transport: State<McpTransportKind>) -> impl IntoElement {
    let current = *transport.read();

    Select::new()
        .selected_item(mcp_transport_label(current).to_owned())
        .children(
            [McpTransportKind::Stdio, McpTransportKind::Http]
                .into_iter()
                .map(|value| {
                    MenuItem::new()
                        .selected(current == value)
                        .on_press(move |_| transport.set(value))
                        .child(mcp_transport_label(value).to_owned())
                        .into()
                }),
        )
}

fn mcp_transport_label(kind: McpTransportKind) -> &'static str {
    match kind {
        McpTransportKind::Stdio => "Local command (stdio)",
        McpTransportKind::Http => "Remote server (HTTP)",
    }
}

/// No list, no add/edit `Mode` — unlike Profiles/MCP servers, there is
/// exactly one of each credential, so this is always just the form,
/// pre-loaded with whatever's currently saved.
fn integrations_popup(
    popup: Popup,
    engine: Engine,
    integrations: IntegrationsConfig,
    fields: IntegrationsFields,
    mut show: State<bool>,
) -> Popup {
    popup
        .child(PopupTitle::new("Integrations".to_owned()))
        .child(PopupContent::new().child(integrations_form(engine.clone(), integrations, fields)))
        .child(
            PopupButtons::new()
                .child(
                    Button::new()
                        .outline()
                        .on_press(move |_| show.set(false))
                        .child("Close"),
                )
                .child(
                    Button::new()
                        .filled()
                        .on_press(move |_| engine.save_integrations(fields.to_config()))
                        .child("Save"),
                ),
        )
}

fn integrations_form(
    engine: Engine,
    integrations: IntegrationsConfig,
    fields: IntegrationsFields,
) -> impl IntoElement {
    let use_gh_cli = *fields.github_use_gh_cli.read();

    rect()
        .width(Size::fill())
        .vertical()
        .spacing(theme::SPACE_SM)
        .child(
            label()
                .text(
                    "Saved machine-wide, in plain text — the same as API keys under \
                     \"API configurations\". Leave a field blank to turn that integration's \
                     tools off. \"Login\" opens your browser to sign in there instead of \
                     pasting a token by hand.",
                )
                .color(theme::TEXT_DIM)
                .font_size(theme::FONT_SMALL),
        )
        .child(field_label("GitHub token"))
        .maybe_child((!use_gh_cli).then(|| {
            credential_row(
                Input::new(fields.github_token)
                    .mode(InputMode::new_password())
                    .placeholder("ghp_...")
                    .width(Size::fill()),
                login_button(
                    engine.clone(),
                    OAuthProvider::GitHub,
                    fields.github_pending,
                    integrations.github_token.is_some(),
                ),
            )
        }))
        .maybe_child(use_gh_cli.then(|| {
            label()
                .text(
                    "Using your local \"gh\" CLI login (gh auth token) instead of a saved \
                     token — run \"gh auth login\" once outside this app if you haven't.",
                )
                .color(theme::TEXT_DIM)
                .font_size(theme::FONT_SMALL)
        }))
        .child(gh_cli_toggle(fields.github_use_gh_cli))
        // Login goes through PKCE, which forfeits bot-token scopes on
        // Slack's side — the result is a `xoxp-` user token, not `xoxb-`,
        // hence "token" rather than "bot token" here (the manual-paste
        // path still accepts either; `architect-slack`'s own `Authorization:
        // Bearer` call doesn't care which kind it is).
        .child(field_label("Slack token"))
        .child(credential_row(
            Input::new(fields.slack_token)
                .mode(InputMode::new_password())
                .placeholder("xoxb-... or xoxp-...")
                .width(Size::fill()),
            login_button(
                engine.clone(),
                OAuthProvider::Slack,
                fields.slack_pending,
                integrations.slack_token.is_some(),
            ),
        ))
        .child(field_label("Linear API key"))
        .child(credential_row(
            Input::new(fields.linear_api_key)
                .mode(InputMode::new_password())
                .placeholder("lin_api_...")
                .width(Size::fill()),
            login_button(
                engine,
                OAuthProvider::Linear,
                fields.linear_pending,
                integrations.linear_api_key.is_some(),
            ),
        ))
}

/// A field's `Input` plus its "Login" button, side by side — flex content
/// so the input claims the row and the button keeps its natural width,
/// same reasoning `mcp_row`/`profile_row` already use for a name column
/// sharing a row with action buttons.
fn credential_row(input: impl IntoElement, login: impl IntoElement) -> impl IntoElement {
    rect()
        .width(Size::fill())
        .horizontal()
        .content(Content::Flex)
        .cross_align(Alignment::Center)
        .spacing(theme::SPACE_SM)
        .child(rect().width(Size::flex(1.0)).child(input))
        .child(login)
}

/// Flips local state only — takes effect on the next "Save", same as every
/// other field in this form. No dedicated checkbox widget exists in this
/// codebase; a relabeled `Button` is the same toggle convention `mcp_row`'s
/// Enabled/Disabled button already uses.
fn gh_cli_toggle(mut use_gh_cli: State<bool>) -> impl IntoElement {
    let is_on = *use_gh_cli.read();

    Button::new()
        .outline()
        .on_press(move |_| use_gh_cli.set(!is_on))
        .child(if is_on {
            "Using gh CLI (click to paste a token instead)"
        } else {
            "Use gh CLI instead of a token"
        })
}

/// Once `connected` (a saved token exists, however it got there — Login or
/// a manual paste+Save), the button disappears in favor of a plain
/// checkmark: nothing left to click, since re-running Login or pasting a
/// fresh token both still work through the field/button underneath once
/// the saved value is cleared.
fn login_button(
    engine: Engine,
    provider: OAuthProvider,
    mut pending: State<bool>,
    connected: bool,
) -> impl IntoElement {
    let is_pending = *pending.read();

    rect()
        .maybe_child((connected && !is_pending).then(|| {
            label()
                .text("\u{2713} Connected")
                .color(theme::SUCCESS)
                .font_size(theme::FONT_SMALL)
        }))
        .maybe_child((!connected || is_pending).then(|| {
            Button::new()
                .outline()
                .on_press(move |_| {
                    pending.set(true);
                    engine.start_oauth_login(provider);
                })
                .child(if is_pending {
                    "Waiting for browser…"
                } else {
                    "Login"
                })
        }))
}

fn docs_popup(popup: Popup, engine: Engine, fields: DocsFields, mut show: State<bool>) -> Popup {
    popup
        .child(PopupTitle::new("Documentation".to_owned()))
        .child(PopupContent::new().child(docs_form(engine.clone(), fields)))
        .child(
            PopupButtons::new()
                .child(
                    Button::new()
                        .outline()
                        .on_press(move |_| show.set(false))
                        .child("Close"),
                )
                .child(
                    Button::new()
                        .filled()
                        .on_press(move |_| engine.save_docs_config(fields.to_config()))
                        .child("Save"),
                ),
        )
}

fn docs_form(engine: Engine, fields: DocsFields) -> impl IntoElement {
    let enabled = *fields.enabled.read();

    rect()
        .width(Size::fill())
        .vertical()
        .spacing(theme::SPACE_SM)
        .child(
            label()
                .text(
                    "Gives the model write_doc/edit_doc/read_doc/search_docs/list_docs/ \
                     scaffold_docs tools, backed by a local Obsidian vault — a plain markdown \
                     folder you can open in Obsidian directly. Notion and Jira drivers are \
                     planned; only Obsidian is available today.",
                )
                .color(theme::TEXT_DIM)
                .font_size(theme::FONT_SMALL),
        )
        .child(docs_enabled_toggle(fields.enabled))
        .maybe_child(enabled.then(|| {
            rect()
                .width(Size::fill())
                .vertical()
                .spacing(theme::SPACE_SM)
                .child(field_label("Vault path"))
                .child(
                    Input::new(fields.vault_path)
                        .placeholder(vault_placeholder(&engine))
                        .width(Size::fill()),
                )
                .child(
                    label()
                        .text(
                            "Leave blank to use the \"docs\" folder inside this workspace \
                             (created automatically the first time a doc is written).",
                        )
                        .color(theme::TEXT_DIM)
                        .font_size(theme::FONT_SMALL),
                )
        }))
}

fn vault_placeholder(engine: &Engine) -> String {
    engine
        .config()
        .workspace_root
        .join("docs")
        .display()
        .to_string()
}

/// Same relabeled-`Button` toggle convention as `gh_cli_toggle`.
fn docs_enabled_toggle(mut enabled: State<bool>) -> impl IntoElement {
    let is_on = *enabled.read();

    Button::new()
        .outline()
        .on_press(move |_| enabled.set(!is_on))
        .child(if is_on {
            "Documentation tools enabled (click to disable)"
        } else {
            "Documentation tools disabled (click to enable)"
        })
}

fn field_label(text: &'static str) -> impl IntoElement {
    label()
        .text(text)
        .color(theme::TEXT_DIM)
        .font_size(theme::FONT_SMALL)
}
