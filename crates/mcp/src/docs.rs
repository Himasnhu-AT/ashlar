//! Documentation for agents ([`ashlar_docs`]) as MCP: the `docs_categories`, `list_docs` and
//! `read_doc` tools and `docs://<path>` resources, and the JSON both they and the CLI print.

pub use ashlar_docs::{CATEGORY_FILE, Category, Doc, Frontmatter, Library, Node, Source, write};
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

/// The one argument a documentation prompt takes.
pub const BRIEF: &str = "brief";

/// The prompts made from the documents in `category` (and its subcategories): one per page, named
/// by its path inside the category without `.md` (`skills/intro.md` → `intro`), titled and
/// described by its frontmatter, with an optional `brief` argument. Empty for an unknown or empty
/// category.
pub fn prompts(lib: &Library, category: &str) -> Vec<Value> {
    lib.list(Some(category), None, true)
        .into_iter()
        .filter_map(|d| {
            Some(json!({
                "name": prompt_name(category, &d.path)?,
                "title": d.title,
                "description": d.description,
                "arguments": [{
                    "name": BRIEF,
                    "description": "The job in the person's words: what to make, from what, how long, in which shape, for whom (optional).",
                    "required": false,
                }],
            }))
        })
        .collect()
}

/// `skills/sub/intro.md` → `sub/intro` for the category `skills`.
fn prompt_name(category: &str, path: &str) -> Option<String> {
    let inside = if category.is_empty() { Some(path) } else { path.strip_prefix(category).and_then(|r| r.strip_prefix('/')) }?;
    Some(inside.strip_suffix(".md").unwrap_or(inside).to_string())
}

/// Why a prompt could not be made: the message for a JSON-RPC invalid-params error.
pub type PromptError = String;

/// `prompts/get`: the page behind prompt `name` in `category` as one user message (its body
/// without frontmatter, said to come from the documentation, and the `brief` when one is given).
pub fn prompt(lib: &Library, category: &str, name: &str, arguments: Option<&Value>) -> Result<Value, PromptError> {
    let args = match arguments {
        None | Some(Value::Null) => serde_json::Map::new(),
        Some(Value::Object(o)) => o.clone(),
        Some(_) => return Err("`arguments` must be an object of strings".into()),
    };
    if let Some(other) = args.keys().find(|k| k.as_str() != BRIEF) {
        return Err(format!("unknown argument `{other}` for prompt `{name}` (it takes: {BRIEF})"));
    }
    let brief = match args.get(BRIEF) {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.trim()).filter(|s| !s.is_empty()),
        Some(_) => return Err(format!("`{BRIEF}` must be a string")),
    };
    let docs = lib.list(Some(category), None, true);
    let Some(doc) = docs.iter().find(|d| prompt_name(category, &d.path).as_deref() == Some(name)) else {
        let names: Vec<String> = docs.iter().filter_map(|d| prompt_name(category, &d.path)).collect();
        let close = ashlar_command::closest(name, names.iter().map(String::as_str), 3);
        let hint = if close.is_empty() {
            format!("; prompts/list names them ({})", names.join(", "))
        } else {
            format!("; did you mean {}?", close.iter().map(|c| format!("`{c}`")).collect::<Vec<_>>().join(", "))
        };
        return Err(format!("no prompt `{name}`{hint}"));
    };
    let body = Frontmatter::parse(&doc.text).body.trim();
    let dir = doc.path.rsplit_once('/').map_or("", |(d, _)| d);
    let mut text = format!(
        "Do this job by following the guide below, from this app's documentation (`{}`). Pages it links to are read with `read_doc`; \
         their paths are relative to `{dir}/`.\n\n{body}\n\n",
        doc.path
    );
    match brief {
        Some(b) => text.push_str(&format!("## The brief\n\n{b}\n")),
        None => text.push_str("No brief was given: ask for what the guide needs before you start.\n"),
    }
    Ok(json!({
        "description": doc.description,
        "messages": [{"role": "user", "content": {"type": "text", "text": text}}],
    }))
}
