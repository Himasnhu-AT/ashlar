//! Documentation for agents ([`ashlar_docs`]) as MCP: the `docs_categories`, `list_docs` and
//! `read_doc` tools and `docs://<path>` resources, and the JSON both they and the CLI print.

pub use ashlar_docs::{CATEGORY_FILE, Category, Doc, Frontmatter, Library, Node, Source};
use serde_json::{Value, json};

/// A category tree: path, title, description, document counts, subcategories.
pub fn node_json(n: &Node) -> Value {
    json!({
        "path": n.path,
        "title": n.title,
        "description": n.description,
        "docs": n.docs,
        "total": n.total,
        "subcategories": n.children.iter().map(node_json).collect::<Vec<_>>(),
    })
}

/// One document without its text.
pub fn doc_json(d: &Doc) -> Value {
    let mut v = json!({
        "path": d.path,
        "title": d.title,
        "description": d.description,
        "category": d.category,
        "source": d.source.as_str(),
    });
    if !d.tags.is_empty()
        && let Some(o) = v.as_object_mut()
    {
        o.insert("tags".into(), json!(d.tags));
    }
    v
}

/// A category: what it is, its overview, its subcategories and the documents directly in it.
pub fn category_json(lib: &Library, c: &Category) -> Value {
    let subs = lib.tree(&c.path).map(|n| n.children.iter().map(node_json).collect::<Vec<_>>()).unwrap_or_default();
    json!({
        "path": c.path,
        "title": c.title,
        "description": c.description,
        "overview": c.overview,
        "subcategories": subs,
        "docs": lib.list(Some(&c.path), None, false).into_iter().map(doc_json).collect::<Vec<_>>(),
    })
}
