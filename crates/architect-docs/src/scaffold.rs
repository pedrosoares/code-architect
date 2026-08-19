//! The default knowledge-base skeleton `scaffold_docs` creates.
//!
//! Only the always-useful, top-of-tree docs are seeded here — `01-domains/`,
//! `02-flows/`, `03-business-rules/`, `04-integrations/`, and `06-decisions/`
//! stay empty until `write_doc` creates the first doc in each, the same way
//! an ordinary directory is created lazily by the first file written into it.

pub struct SkeletonDoc {
    pub path: &'static str,
    pub id: &'static str,
    pub doc_type: &'static str,
    pub title: &'static str,
    pub body: &'static str,
}

pub fn skeleton() -> &'static [SkeletonDoc] {
    &[
        SkeletonDoc {
            path: "00-system/overview.md",
            id: "system.overview",
            doc_type: "system",
            title: "System Overview",
            body: "What this system does and who it's for.\n",
        },
        SkeletonDoc {
            path: "00-system/glossary.md",
            id: "system.glossary",
            doc_type: "system",
            title: "Glossary",
            body: "Ambiguous or domain-specific terms, defined once here rather than \
                   re-explained in every doc that uses them.\n\n\
                   | Term | Definition |\n|---|---|\n",
        },
        SkeletonDoc {
            path: "00-system/architecture.md",
            id: "system.architecture",
            doc_type: "system",
            title: "Architecture",
            body: "The system's overall shape — major components and how they fit together.\n",
        },
        SkeletonDoc {
            path: "00-system/conventions.md",
            id: "system.conventions",
            doc_type: "system",
            title: "Conventions",
            body: "Coding and documentation conventions for this project.\n",
        },
        SkeletonDoc {
            path: "05-data/entities.md",
            id: "data.entities",
            doc_type: "index",
            title: "Entities",
            body: "Index of entities. Individual entities live under \
                   `01-domains/<domain>/entities.md` — use `list_docs` with \
                   `doc_type: \"entity\"` for the current list.\n",
        },
        SkeletonDoc {
            path: "05-data/relationships.md",
            id: "data.relationships",
            doc_type: "index",
            title: "Entity Relationships",
            body: "How entities relate to each other, beyond what each entity's own \
                   doc already states.\n",
        },
        SkeletonDoc {
            path: "05-data/invariants.md",
            id: "data.invariants",
            doc_type: "index",
            title: "System Invariants",
            body: "Rules that must always remain true across the whole system — the \
                   ones an agent should never assume it's safe to break.\n",
        },
        SkeletonDoc {
            path: "99-index/domain-map.md",
            id: "index.domain-map",
            doc_type: "index",
            title: "Domain Map",
            body: "A human-written map of the domains, for orientation. `list_docs` with \
                   `doc_type: \"domain\"` gives the current, generated version of this — \
                   prefer that when precision matters.\n",
        },
        SkeletonDoc {
            path: "99-index/flow-map.md",
            id: "index.flow-map",
            doc_type: "index",
            title: "Flow Map",
            body: "A human-written map of the flows, for orientation. `list_docs` with \
                   `doc_type: \"flow\"` gives the current, generated version of this.\n",
        },
        SkeletonDoc {
            path: "99-index/dependency-map.md",
            id: "index.dependency-map",
            doc_type: "index",
            title: "Dependency Map",
            body: "A human-written map of cross-domain dependencies, for orientation. \
                   Each doc's own `depends_on` (visible via `list_docs`) is the current, \
                   generated version of this.\n",
        },
    ]
}
