//! Right panel: workspace inspector, tabbed.
//!
//! Reads the active conversation's [`Row::Tool`] rows and `file_changes` —
//! nothing here decides what happened during a turn, it only shows it.

use std::path::{Path, PathBuf};

use architect_core::{FileChangeEntry, Plan, StepStatus};
use architect_session::SessionId;
use architect_tools::ProcessStatus;
use architect_ui::{theme, widgets::PanelHeader};
use freya::code_editor::{
    EditorLanguage, EditorSyntaxTheme, Rope, SyntaxBlocks, SyntaxHighlighter, TextNode,
};
use freya::components::{
    Button, ButtonSegment, Popup, PopupButtons, PopupContent, PopupTitle, ScrollView,
    SegmentedButton,
};
use freya::prelude::*;
use similar::{ChangeTag, TextDiff};

use crate::{
    engine::Engine,
    panels::{InspectorCollapsed, ScrollToToolCall},
    state::{BrowserSummary, ProcessSummary, Row, ToolStatus, Transcript},
};

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Files,
    Tools,
    Diff,
    Processes,
    Browser,
    Plan,
}

impl Tab {
    const ALL: [Tab; 6] = [
        Tab::Files,
        Tab::Tools,
        Tab::Diff,
        Tab::Processes,
        Tab::Browser,
        Tab::Plan,
    ];

    fn label(self) -> &'static str {
        match self {
            Tab::Files => "Files",
            Tab::Tools => "Tools",
            Tab::Diff => "Diff",
            Tab::Processes => "Processes",
            Tab::Browser => "Browser",
            Tab::Plan => "Plan",
        }
    }

    /// Empty state shown until the agent starts reporting activity.
    fn empty_state(self) -> &'static str {
        match self {
            Tab::Files => "No files touched yet.",
            Tab::Tools => "No tool calls yet.",
            Tab::Diff => "No files touched yet.",
            Tab::Processes => "No processes started yet.",
            Tab::Browser => "No browser activity yet.",
            Tab::Plan => "No plan saved yet.",
        }
    }
}

#[derive(PartialEq)]
pub struct InspectorPanel;

impl Component for InspectorPanel {
    fn render(&self) -> impl IntoElement {
        let engine = consume_context::<Engine>();
        let transcript = consume_context::<State<Transcript>>();
        let scroll_target = consume_context::<ScrollToToolCall>().0;
        let mut collapsed = consume_context::<InspectorCollapsed>().0;

        let mut selected_tab = use_state(|| Tab::Files);
        let selected_file = use_state::<Option<PathBuf>>(|| None);
        let rollback_target = use_state::<Option<PathBuf>>(|| None);
        let expanded_process = use_state::<Option<String>>(|| None);

        let mut tabs = SegmentedButton::new();
        for tab in Tab::ALL {
            tabs = tabs.child(
                ButtonSegment::new()
                    .selected(*selected_tab.read() == tab)
                    .on_press(move |_| selected_tab.set(tab))
                    .child(label().text(tab.label()).max_lines(1)),
            );
        }

        let (rows, file_changes, plan) = {
            let transcript = transcript.read();
            match transcript.active_conversation() {
                Some(conversation) => (
                    conversation.rows.clone(),
                    conversation.file_changes.clone(),
                    conversation.plan.clone(),
                ),
                None => (Vec::new(), Vec::new(), None),
            }
        };
        let files = dedupe_file_changes(&file_changes);
        // Global, unlike `rows`/`file_changes` above: a process isn't owned
        // by whichever conversation happened to start it, the same way a
        // connected MCP server isn't — see `ProcessSummary`'s doc comment.
        let processes = transcript.read().processes.clone();
        // Global too — the engine owns exactly one headless browser, so the
        // summary is a single `Option`, not per-conversation (see
        // `BrowserSummary`'s doc comment). `None` means the browser has never
        // been touched this run.
        let browser = transcript.read().browser.clone();

        let session = transcript
            .read()
            .active_session
            .expect("Transcript::default always starts one conversation");

        let body = match *selected_tab.read() {
            Tab::Tools => tools_tab(&rows, scroll_target),
            Tab::Files => files_tab(
                &files,
                engine.config().workspace_root.clone(),
                selected_tab,
                selected_file,
                rollback_target,
            ),
            Tab::Diff => diff_tab(&files, selected_file.read().clone()),
            Tab::Processes => processes_tab(&processes, expanded_process),
            Tab::Browser => browser_tab(browser.as_ref()),
            Tab::Plan => plan_tab(plan.as_ref()),
        };

        rect()
            .expanded()
            .vertical()
            .background(theme::SURFACE)
            .child(
                PanelHeader::new("Inspector").trailing(
                    Button::new()
                        .outline()
                        .on_press(move |_| collapsed.set(true))
                        .child(label().text("\u{203A}").font_size(theme::FONT_SMALL)),
                ),
            )
            .child(
                rect()
                    .width(Size::fill())
                    .padding(Gaps::new_symmetric(0.0, theme::SPACE_SM))
                    // The tab bar is horizontally scrollable: the Inspector
                    // panel is user-resizable (down to the 32px collapsed
                    // width) and content-width segments don't wrap, so a
                    // narrow panel can't always fit all of them. Scrolling —
                    // rather than clipping, truncating labels, or shrinking the
                    // panel to the tabs — keeps every tab reachable at any
                    // width, and at scroll 0 the leftmost tabs keep their
                    // usual positions, so a wide panel looks unchanged.
                    .child(
                        ScrollView::new()
                            .direction(Direction::Horizontal)
                            .show_scrollbar(false)
                            .child(tabs),
                    ),
            )
            .child(rect().width(Size::fill()).height(Size::fill()).child(body))
            .child(rollback_popup(engine, session, &files, rollback_target))
    }
}

fn empty_message(text: &'static str) -> Element {
    rect()
        .expanded()
        .center()
        .padding(Gaps::new_all(theme::SPACE_MD))
        .child(
            label()
                .text(text)
                .color(theme::TEXT_DIM)
                .font_size(theme::FONT_SMALL),
        )
        .into_element()
}

/// One entry per path that was touched this conversation: the first old
/// content seen for it and the most recent new content/tool/`message_seq` —
/// what actually changed across every edit to that file, not just the last
/// one. The last-seen `message_seq` is also exactly the right rollback
/// anchor for "undo this file's most recent edit": `reverse_to_point`
/// restores that edit's `old_content`, which correctly chains back through
/// any earlier edits to the same file.
fn dedupe_file_changes(changes: &[FileChangeEntry]) -> Vec<FileChangeEntry> {
    let mut by_path: Vec<FileChangeEntry> = Vec::new();
    for entry in changes {
        match by_path
            .iter_mut()
            .find(|existing| existing.change.file_path == entry.change.file_path)
        {
            Some(existing) => {
                existing.change.new_content = entry.change.new_content.clone();
                existing.change.tool_name = entry.change.tool_name;
                existing.message_seq = entry.message_seq;
            }
            None => by_path.push(entry.clone()),
        }
    }
    by_path
}

fn display_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

fn tools_tab(rows: &[Row], mut scroll_target: State<Option<String>>) -> Element {
    let tools: Vec<&Row> = rows
        .iter()
        .filter(|row| matches!(row, Row::Tool { .. }))
        .collect();
    if tools.is_empty() {
        return empty_message(Tab::Tools.empty_state());
    }

    let mut list = rect()
        .width(Size::fill())
        .vertical()
        .spacing(theme::SPACE_SM)
        .padding(Gaps::new_all(theme::SPACE_MD));

    for (index, row) in tools.into_iter().enumerate() {
        let Row::Tool {
            id,
            name,
            arguments,
            status,
            ..
        } = row
        else {
            unreachable!("filtered to Row::Tool above")
        };
        let (mark, color) = match status {
            ToolStatus::Running => ("\u{25CF}", theme::TEXT_DIM),
            ToolStatus::Ok => ("\u{2713}", theme::SUCCESS),
            ToolStatus::Failed => ("\u{2717}", theme::ERROR),
        };
        let target_id = id.clone();

        list = list.child(
            rect()
                .key(index)
                .width(Size::fill())
                .horizontal()
                .content(Content::Flex)
                .cross_align(Alignment::Center)
                .spacing(theme::SPACE_SM)
                .padding(Gaps::new_symmetric(8.0, 10.0))
                .background(theme::SURFACE_RAISED)
                .border(Border::new().fill(theme::BORDER).width(1.0))
                .rounded()
                .on_press(move |_| scroll_target.set(Some(target_id.clone())))
                .child(
                    rect()
                        .horizontal()
                        .cross_align(Alignment::Center)
                        .spacing(8.0)
                        .child(label().text(mark).color(color).font_size(theme::FONT_SMALL))
                        .child(label().text(name.clone()).font_family(theme::FONT_MONO)),
                )
                .child(
                    label()
                        .width(Size::flex(1.0))
                        .text(arguments.clone())
                        .color(theme::TEXT_DIM)
                        .font_family(theme::FONT_MONO)
                        .font_size(theme::FONT_SMALL)
                        .text_align(TextAlign::Right)
                        .max_lines(1)
                        .text_overflow(TextOverflow::Ellipsis),
                ),
        );
    }

    rect()
        .width(Size::fill())
        .height(Size::fill())
        .child(ScrollView::new().child(list))
        .into_element()
}

/// One row per process the agent has started (via `start_process`), newest
/// last — same list-of-rows shape `tools_tab` above already uses. Clicking
/// a row expands it in place to show its accumulated log, instead of
/// switching tabs the way Files→Diff does: there's no natural second view
/// for a process the way a diff is for a file, just more or less of the
/// same log.
fn processes_tab(processes: &[ProcessSummary], mut expanded: State<Option<String>>) -> Element {
    if processes.is_empty() {
        return empty_message(Tab::Processes.empty_state());
    }

    let mut list = rect()
        .width(Size::fill())
        .vertical()
        .spacing(theme::SPACE_SM)
        .padding(Gaps::new_all(theme::SPACE_MD));

    for (index, process) in processes.iter().enumerate() {
        let (mark, color) = match &process.status {
            ProcessStatus::Running => ("\u{25CF}", theme::TEXT_DIM),
            ProcessStatus::Exited(0) | ProcessStatus::Stopped => ("\u{2713}", theme::SUCCESS),
            ProcessStatus::Exited(_) | ProcessStatus::Failed(_) => ("\u{2717}", theme::ERROR),
        };
        let is_expanded = expanded.read().as_deref() == Some(process.id.as_str());
        let target_id = process.id.clone();
        let status_text = process.status.to_string();
        let log_text = if process.log.is_empty() {
            "(no output yet)".to_owned()
        } else {
            process.log.clone()
        };

        list = list.child(
            rect()
                .key(index)
                .width(Size::fill())
                .vertical()
                .spacing(theme::SPACE_SM)
                .child(
                    rect()
                        .width(Size::fill())
                        .horizontal()
                        .content(Content::Flex)
                        .cross_align(Alignment::Center)
                        .spacing(theme::SPACE_SM)
                        .padding(Gaps::new_symmetric(8.0, 10.0))
                        .background(theme::SURFACE_RAISED)
                        .border(Border::new().fill(theme::BORDER).width(1.0))
                        .rounded()
                        .on_press(move |_| {
                            expanded.set(if is_expanded {
                                None
                            } else {
                                Some(target_id.clone())
                            });
                        })
                        .child(
                            rect()
                                .horizontal()
                                .cross_align(Alignment::Center)
                                .spacing(8.0)
                                .child(label().text(mark).color(color).font_size(theme::FONT_SMALL))
                                .child(
                                    label()
                                        .text(process.id.clone())
                                        .font_family(theme::FONT_MONO),
                                ),
                        )
                        .child(
                            label()
                                .width(Size::flex(1.0))
                                .text(process.command.clone())
                                .color(theme::TEXT_DIM)
                                .font_family(theme::FONT_MONO)
                                .font_size(theme::FONT_SMALL)
                                .text_align(TextAlign::Right)
                                .max_lines(1)
                                .text_overflow(TextOverflow::Ellipsis),
                        ),
                )
                .maybe_child(is_expanded.then(|| {
                    rect()
                        .width(Size::fill())
                        .vertical()
                        .spacing(theme::SPACE_SM)
                        .padding(Gaps::new_all(theme::SPACE_SM))
                        .background(theme::SURFACE)
                        .border(Border::new().fill(theme::BORDER).width(1.0))
                        .rounded()
                        .child(
                            label()
                                .text(status_text)
                                .color(theme::TEXT_DIM)
                                .font_size(theme::FONT_SMALL),
                        )
                        .child(
                            label()
                                .text(log_text)
                                .font_family(theme::FONT_MONO)
                                .font_size(theme::FONT_SMALL),
                        )
                })),
        );
    }

    rect()
        .width(Size::fill())
        .height(Size::fill())
        .child(ScrollView::new().child(list))
        .into_element()
}

/// The one-and-only headless Firefox, shown the way the Processes tab shows
/// its processes — a single status block, not a list, because there is never
/// more than one. Read-only: unlike a process row there is nothing to click
/// (no per-tab log to expand; the browser's console is surfaced through the
/// `firefox_logs` tool, not here), so this is a status display like the Plan
/// tab.
fn browser_tab(browser: Option<&BrowserSummary>) -> Element {
    let Some(browser) = browser else {
        return empty_message(Tab::Browser.empty_state());
    };

    let (mark, color, status) = if browser.driver_running && browser.has_session {
        ("\u{25CF}", theme::TEXT_DIM, "running".to_owned())
    } else if browser.driver_running {
        // The geckodriver process is up but no page has been opened yet.
        (
            "\u{25CB}",
            theme::TEXT_DIM,
            "driver running — no session".to_owned(),
        )
    } else if browser.last_error.is_some() {
        ("\u{2717}", theme::ERROR, "stopped".to_owned())
    } else {
        ("\u{25CB}", theme::TEXT_DIM, "stopped".to_owned())
    };

    let mut list = rect()
        .width(Size::fill())
        .vertical()
        .spacing(theme::SPACE_SM)
        .padding(Gaps::new_all(theme::SPACE_MD))
        .child(
            rect()
                .width(Size::fill())
                .horizontal()
                .cross_align(Alignment::Center)
                .spacing(8.0)
                .padding(Gaps::new_symmetric(8.0, 10.0))
                .background(theme::SURFACE_RAISED)
                .border(Border::new().fill(theme::BORDER).width(1.0))
                .rounded()
                .child(label().text(mark).color(color).font_size(theme::FONT_SMALL))
                .child(
                    label()
                        .text(status)
                        .font_family(theme::FONT_MONO)
                        .font_size(theme::FONT_SMALL),
                ),
        );

    if let Some(port) = browser.port {
        list = list.child(
            label()
                .text(format!("driver 127.0.0.1:{port}"))
                .color(theme::TEXT_DIM)
                .font_family(theme::FONT_MONO)
                .font_size(theme::FONT_SMALL),
        );
    }
    if let Some(url) = &browser.url {
        list = list.child(
            rect()
                .width(Size::fill())
                .vertical()
                .spacing(2.0)
                .child(
                    label()
                        .text("page")
                        .color(theme::TEXT_DIM)
                        .font_family(theme::FONT_MONO)
                        .font_size(theme::FONT_SMALL),
                )
                .child(
                    label()
                        .text(url.clone())
                        .color(theme::TEXT)
                        .font_family(theme::FONT_MONO)
                        .font_size(theme::FONT_SMALL)
                        .max_lines(1)
                        .text_overflow(TextOverflow::Ellipsis),
                )
                .maybe_child(browser.title.as_ref().map(|title| {
                    label()
                        .text(title.clone())
                        .color(theme::TEXT_DIM)
                        .font_size(theme::FONT_SMALL)
                        .max_lines(1)
                        .text_overflow(TextOverflow::Ellipsis)
                })),
        );
    }
    if let Some(error) = &browser.last_error {
        list = list.child(
            label()
                .text(error.clone())
                .color(theme::ERROR)
                .font_family(theme::FONT_MONO)
                .font_size(theme::FONT_SMALL)
                .max_lines(3),
        );
    }

    rect()
        .width(Size::fill())
        .height(Size::fill())
        .child(ScrollView::new().child(list))
        .into_element()
}

fn plan_status_glyph(status: StepStatus) -> (&'static str, Color) {
    match status {
        StepStatus::Pending => ("\u{25CB}", theme::TEXT_DIM),
        StepStatus::InProgress => ("\u{25CF}", theme::TEXT_DIM),
        StepStatus::Completed => ("\u{2713}", theme::SUCCESS),
    }
}

/// One row per step, sub-steps indented beneath in a smaller/dimmer style
/// — unlike `tools_tab`/`processes_tab`, no click/expand interaction:
/// a step's description already is its whole content, there's no
/// secondary detail to reveal.
fn plan_tab(plan: Option<&Plan>) -> Element {
    let Some(plan) = plan else {
        return empty_message(Tab::Plan.empty_state());
    };
    if plan.steps.is_empty() {
        return empty_message(Tab::Plan.empty_state());
    }

    let mut list = rect()
        .width(Size::fill())
        .vertical()
        .spacing(theme::SPACE_SM)
        .padding(Gaps::new_all(theme::SPACE_MD));

    if let Some(goal) = &plan.goal {
        list = list.child(
            label()
                .text(format!("Goal: {goal}"))
                .font_weight(FontWeight::BOLD)
                .font_size(theme::FONT_SMALL),
        );
    }

    for (index, step) in plan.steps.iter().enumerate() {
        let (mark, color) = plan_status_glyph(step.status);

        let mut step_block = rect()
            .key(index)
            .width(Size::fill())
            .vertical()
            .spacing(4.0)
            .child(
                rect()
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(8.0)
                    .child(label().text(mark).color(color).font_size(theme::FONT_SMALL))
                    .child(label().text(step.description.clone())),
            );

        for substep in &step.substeps {
            let (sub_mark, sub_color) = plan_status_glyph(substep.status);
            step_block = step_block.child(
                rect()
                    .padding(Gaps::new(0.0, 0.0, 0.0, 28.0))
                    .horizontal()
                    .cross_align(Alignment::Center)
                    .spacing(8.0)
                    .child(
                        label()
                            .text(sub_mark)
                            .color(sub_color)
                            .font_size(theme::FONT_SMALL),
                    )
                    .child(
                        label()
                            .text(substep.description.clone())
                            .color(theme::TEXT_DIM)
                            .font_size(theme::FONT_SMALL),
                    ),
            );
        }

        list = list.child(step_block);
    }

    rect()
        .width(Size::fill())
        .height(Size::fill())
        .child(ScrollView::new().child(list))
        .into_element()
}

fn files_tab(
    files: &[FileChangeEntry],
    workspace_root: PathBuf,
    mut selected_tab: State<Tab>,
    mut selected_file: State<Option<PathBuf>>,
    mut rollback_target: State<Option<PathBuf>>,
) -> Element {
    if files.is_empty() {
        return empty_message(Tab::Files.empty_state());
    }

    let mut list = rect()
        .width(Size::fill())
        .vertical()
        .spacing(theme::SPACE_SM)
        .padding(Gaps::new_all(theme::SPACE_MD));

    for (index, entry) in files.iter().enumerate() {
        let path = entry.change.file_path.clone();
        let display = display_path(&workspace_root, &path);
        let tool_name = entry.change.tool_name;
        let rollback_path = path.clone();

        list = list.child(
            rect()
                .key(index)
                .width(Size::fill())
                .horizontal()
                .content(Content::Flex)
                .cross_align(Alignment::Center)
                .spacing(theme::SPACE_SM)
                .padding(Gaps::new_symmetric(10.0, theme::SPACE_MD))
                .background(theme::SURFACE_RAISED)
                .rounded_lg()
                .on_press(move |_| {
                    // Both set together: picking a file is always "go look
                    // at its diff," never a selection with no visible effect.
                    selected_file.set(Some(path.clone()));
                    selected_tab.set(Tab::Diff);
                })
                .child(
                    label()
                        .width(Size::flex(1.0))
                        .text(display)
                        .max_lines(1)
                        .text_overflow(TextOverflow::Ellipsis),
                )
                .child(
                    label()
                        .text(tool_name)
                        .color(theme::TEXT_DIM)
                        .font_size(theme::FONT_SMALL),
                )
                .child(
                    Button::new()
                        .outline()
                        .on_press(move |e: Event<PressEventData>| {
                            // Rolling back must not also select the row it's
                            // sitting in — the row's own `on_press` would
                            // otherwise fire right alongside this one.
                            e.stop_propagation();
                            rollback_target.set(Some(rollback_path.clone()));
                        })
                        .child("Roll back"),
                ),
        );
    }

    rect()
        .width(Size::fill())
        .height(Size::fill())
        .child(ScrollView::new().child(list))
        .into_element()
}

/// The confirmation dialog for rolling back to before one file's most
/// recent edit — same `Popup`/`PopupTitle`/`PopupContent`/`PopupButtons`
/// shape `settings.rs` already uses. Rendered unconditionally (like
/// `SettingsPanel`); `.maybe(bool, ..)` is what actually gates it on
/// `rollback_target` being `Some`.
fn rollback_popup(
    engine: Engine,
    session: SessionId,
    files: &[FileChangeEntry],
    mut rollback_target: State<Option<PathBuf>>,
) -> Element {
    let target = rollback_target.read().clone();
    let up_to_seq = target
        .as_ref()
        .and_then(|path| files.iter().find(|entry| entry.change.file_path == *path))
        .map(|entry| entry.message_seq - 1);

    Popup::new()
        .on_close_request(move |_| rollback_target.set(None))
        .maybe(target.is_some(), move |popup| {
            popup
                .child(PopupTitle::new(
                    "Roll back to before this change?".to_owned(),
                ))
                .child(
                    PopupContent::new().child(
                        label()
                            .text(
                                "This undoes every file change made after this point in the \
                             conversation, not just this file. Conversation messages are kept.",
                            )
                            .color(theme::TEXT_DIM),
                    ),
                )
                .child(
                    PopupButtons::new()
                        .child(
                            Button::new()
                                .outline()
                                .on_press(move |_| rollback_target.set(None))
                                .child("Cancel"),
                        )
                        .child(
                            Button::new()
                                .filled()
                                .on_press(move |_| {
                                    if let Some(up_to_seq) = up_to_seq {
                                        engine.rollback(session, up_to_seq);
                                    }
                                    rollback_target.set(None);
                                })
                                .child("Roll Back"),
                        ),
                )
        })
        .into_element()
}

/// A language's tree-sitter grammar and highlight query, resolved from a
/// file's extension — `None` for anything not in this small, deliberately
/// limited set, which falls back to plain (still diff-colored) text further
/// down.
fn language_for(path: &Path) -> Option<EditorLanguage> {
    let extension = path.extension()?.to_str()?;
    let (language, query): (tree_sitter::Language, &str) = match extension {
        "rs" => (
            tree_sitter_rust::LANGUAGE.into(),
            tree_sitter_rust::HIGHLIGHTS_QUERY,
        ),
        "py" => (
            tree_sitter_python::LANGUAGE.into(),
            tree_sitter_python::HIGHLIGHTS_QUERY,
        ),
        "js" | "mjs" | "cjs" => (
            tree_sitter_javascript::LANGUAGE.into(),
            tree_sitter_javascript::HIGHLIGHT_QUERY,
        ),
        // The TSX grammar is a superset of TypeScript's that also parses
        // JSX — used for `.jsx` too rather than pulling in a second query.
        "ts" | "mts" | "cts" => (
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
        ),
        "tsx" | "jsx" => (
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
        ),
        "json" => (
            tree_sitter_json::LANGUAGE.into(),
            tree_sitter_json::HIGHLIGHTS_QUERY,
        ),
        "sh" | "bash" => (
            tree_sitter_bash::LANGUAGE.into(),
            tree_sitter_bash::HIGHLIGHT_QUERY,
        ),
        _ => return None,
    };
    Some(EditorLanguage::new(language, query))
}

/// One file's content, tokenized into per-line, per-token `(color, text)`
/// spans — resolved eagerly into owned `String`s since the `Rope` they're
/// sliced from doesn't outlive this call.
fn highlighted_lines(
    text: &str,
    language: &EditorLanguage,
    theme: &EditorSyntaxTheme,
) -> Vec<Vec<(Color, String)>> {
    let rope = Rope::from_str(text);
    let mut highlighter = SyntaxHighlighter::new();
    highlighter.set_language(Some(language), theme);
    let mut blocks = SyntaxBlocks::default();
    highlighter.parse(&rope, &mut blocks, None, theme);

    (0..blocks.len())
        .map(|line| {
            blocks
                .get_line(line)
                .iter()
                .map(|(color, node)| {
                    let text = match node {
                        TextNode::Range(range) => rope.slice(range.clone()).to_string(),
                        TextNode::LineOfChars { len, .. } => " ".repeat(*len),
                    };
                    (*color, text)
                })
                .collect()
        })
        .collect()
}

fn diff_tab(files: &[FileChangeEntry], selected: Option<PathBuf>) -> Element {
    if files.is_empty() {
        return empty_message(Tab::Diff.empty_state());
    }

    let change = selected
        .as_ref()
        .and_then(|path| files.iter().find(|entry| entry.change.file_path == *path))
        .map(|entry| &entry.change);
    let Some(change) = change else {
        return empty_message("Pick a file to see its diff.");
    };

    let old = change.old_content.clone().unwrap_or_default();
    let diff = TextDiff::from_lines(&old, &change.new_content);

    // Highlighted once per side, up front — a `SyntaxHighlighter` parses one
    // whole document, so each diff line can't be tokenized in isolation
    // without breaking multi-line constructs like block comments/strings.
    let language = selected.as_deref().and_then(language_for);
    let syntax_theme = EditorSyntaxTheme::dark();
    let old_lines = language
        .as_ref()
        .map(|language| highlighted_lines(&old, language, &syntax_theme));
    let new_lines = language
        .as_ref()
        .map(|language| highlighted_lines(&change.new_content, language, &syntax_theme));

    let mut list = rect().width(Size::fill()).vertical();
    for (index, entry) in diff.iter_all_changes().enumerate() {
        let (prefix, prefix_color, background, side_lines, line_index) = match entry.tag() {
            ChangeTag::Delete => (
                "-",
                theme::ERROR,
                Some(theme::ERROR_SOFT),
                old_lines.as_ref(),
                entry.old_index(),
            ),
            ChangeTag::Insert => (
                "+",
                theme::SUCCESS,
                Some(theme::SUCCESS_SOFT),
                new_lines.as_ref(),
                entry.new_index(),
            ),
            ChangeTag::Equal => (
                " ",
                theme::TEXT_DIM,
                None,
                new_lines.as_ref(),
                entry.new_index(),
            ),
        };

        let mut row = rect().key(index).width(Size::fill()).horizontal().child(
            label()
                .text(prefix)
                .color(prefix_color)
                .font_family(theme::FONT_MONO)
                .font_size(theme::FONT_SMALL),
        );
        if let Some(background) = background {
            row = row.background(background);
        }

        let spans = side_lines
            .zip(line_index)
            .and_then(|(lines, i)| lines.get(i));
        match spans {
            Some(spans) => {
                for (token_index, (color, text)) in spans.iter().enumerate() {
                    row = row.child(
                        label()
                            .key(token_index)
                            .text(text.clone())
                            .color(*color)
                            .font_family(theme::FONT_MONO)
                            .font_size(theme::FONT_SMALL),
                    );
                }
            }
            // No language for this file, or a line `similar` reports beyond
            // what got tokenized (shouldn't happen, but falls back safely)
            // — the plain diff-colored line this tab always rendered.
            None => {
                row = row.child(
                    label()
                        .text(entry.value().trim_end_matches('\n').to_owned())
                        .color(prefix_color)
                        .font_family(theme::FONT_MONO)
                        .font_size(theme::FONT_SMALL),
                );
            }
        }

        list = list.child(row);
    }

    rect()
        .width(Size::fill())
        .height(Size::fill())
        .child(
            ScrollView::new().child(
                rect()
                    .width(Size::fill())
                    .padding(Gaps::new_all(theme::SPACE_MD))
                    .child(list),
            ),
        )
        .into_element()
}
