#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The library: categories from folders, frontmatter, the tree, listing, reading, the run-time
//! overlay and the build-time table.

use ashlar_docs::{Frontmatter, Library, Source, embed};

const ROOT: &str = "---\ntitle: Library\ndescription: What is here\n---\n\nStart with the skills.\n";
const SKILLS: &str = "---\ntitle: Skills\ndescription: Step by step\n---\nRecipes for whole jobs.\n";
const CUT: &str = "---\ntitle: Cut a scene\ndescription: \"Read when: you split a clip\"\norder: 2\ntags: [edit, \"razor\"]\n---\n# Cut\n\nSplit at the playhead.\n";
const INTRO: &str = "---\ntitle: Start here\ndescription: Read first\norder: 1\n---\n# Intro\n";
const EASES: &str = "---\ntitle: Eases\ndescription: Curves\n---\nbody\n";

fn library() -> Library {
    Library::from_files([
        ("_category.md", ROOT),
        ("skills/_category.md", SKILLS),
        ("skills/cut.md", CUT),
        ("skills/intro.md", INTRO),
        ("reference/motion/eases.md", EASES),
        ("reference/review/gates.md", "# Gates\n\nNo frontmatter.\n"),
    ])
}

#[test]
fn frontmatter_fields_and_body() {
    let f = Frontmatter::parse(CUT);
    assert_eq!(f.title.as_deref(), Some("Cut a scene"));
    assert_eq!(f.description.as_deref(), Some("Read when: you split a clip"));
    assert_eq!(f.order, Some(2));
    assert_eq!(f.tags, ["edit", "razor"]);
    assert_eq!(f.body, "# Cut\n\nSplit at the playhead.\n");
    let none = Frontmatter::parse("# Title\n---\nnot frontmatter\n");
    assert_eq!((none.title, none.body), (None, "# Title\n---\nnot frontmatter\n"));
    let unclosed = Frontmatter::parse("---\ntitle: x\nno end\n");
    assert_eq!((unclosed.title, unclosed.body), (None, "---\ntitle: x\nno end\n"));
}

#[test]
fn categories_come_from_folders_and_category_files_are_not_documents() {
    let lib = library();
    assert_eq!(lib.docs().len(), 4);
    assert_eq!(lib.overview(), Some("Start with the skills."));
    let tree = lib.tree("").unwrap();
    assert_eq!((tree.title.as_str(), tree.total, tree.docs), ("Library", 4, 0));
    let names: Vec<&str> = tree.children.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(names, ["reference", "skills"]);
    let reference = &tree.children[0];
    assert_eq!((reference.title.as_str(), reference.docs, reference.total), ("Reference", 0, 2));
    let subs: Vec<&str> = reference.children.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(subs, ["reference/motion", "reference/review"]);
    let skills = lib.tree("skills/").unwrap();
    assert_eq!((skills.title.as_str(), skills.description.as_str(), skills.docs), ("Skills", "Step by step", 2));
    assert!(lib.tree("nope").is_none());
    // a document without frontmatter takes its first heading as title
    assert_eq!(lib.docs().iter().find(|d| d.path == "reference/review/gates.md").unwrap().title, "Gates");
}

#[test]
fn list_by_category_query_and_order() {
    let lib = library();
    let paths = |v: Vec<&ashlar_docs::Doc>| v.into_iter().map(|d| d.path.clone()).collect::<Vec<_>>();
    assert_eq!(paths(lib.list(Some("skills"), None, true)), ["skills/intro.md", "skills/cut.md"]);
    assert_eq!(paths(lib.list(Some("reference"), None, false)), Vec::<String>::new());
    assert_eq!(paths(lib.list(Some("reference"), None, true)), ["reference/motion/eases.md", "reference/review/gates.md"]);
    assert_eq!(paths(lib.list(None, Some("RAZOR"), true)), ["skills/cut.md"]);
    assert_eq!(paths(lib.list(None, Some("playhead"), true)), ["skills/cut.md"]);
    assert_eq!(lib.list(None, None, true).len(), 4);
}

#[test]
fn read_is_exact_and_verbatim_with_close_matches_on_a_miss() {
    let lib = library();
    assert_eq!(lib.read("skills/cut.md").unwrap(), CUT);
    assert_eq!(lib.read("docs://skills/cut.md").unwrap(), CUT);
    assert_eq!(lib.read("skills/_category.md").unwrap(), SKILLS);
    assert_eq!(lib.read("_category.md").unwrap(), ROOT);
    let e = lib.read("cut.md").unwrap_err();
    assert!(e.contains("`skills/cut.md`"), "{e}");
    let e = lib.read("skills").unwrap_err();
    assert!(e.contains("is a category"), "{e}");
    assert_eq!(lib.read("zzz.md").unwrap_err(), "no document `zzz.md`");
}

#[test]
fn a_user_folder_overlays_the_built_in_pages() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("skills")).unwrap();
    std::fs::create_dir_all(dir.path().join("mine")).unwrap();
    std::fs::write(dir.path().join("skills/cut.md"), "---\ntitle: My cut\ndescription: Mine\n---\n").unwrap();
    std::fs::write(dir.path().join("mine/notes.md"), "---\ntitle: Notes\ndescription: Mine too\n---\n").unwrap();
    std::fs::write(dir.path().join("mine/.hidden.md"), "x").unwrap();
    std::fs::write(dir.path().join("mine/picture.png"), "x").unwrap();
    std::fs::write(dir.path().join("mine/big.md"), vec![b'a'; (ashlar_docs::MAX_FILE_BYTES + 1) as usize]).unwrap();
    let lib = library().overlay_dir(dir.path());
    let cut = lib.docs().iter().find(|d| d.path == "skills/cut.md").unwrap();
    assert_eq!((cut.title.as_str(), cut.source), ("My cut", Source::User));
    assert_eq!(cut.source.as_str(), "yours");
    let intro = lib.docs().iter().find(|d| d.path == "skills/intro.md").unwrap();
    assert_eq!(intro.source.as_str(), "built-in");
    let mine: Vec<&str> = lib.list(Some("mine"), None, true).iter().map(|d| d.path.as_str()).collect();
    assert_eq!(mine, ["mine/notes.md"]);
    assert_eq!(lib.tree("").unwrap().total, 5);
    // a missing folder adds nothing
    assert_eq!(library().overlay_dir(&dir.path().join("absent")), library());
}

#[cfg(unix)]
#[test]
fn symbolic_links_are_skipped() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.md"), "# Secret").unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("page.md"), "# Page").unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("linked")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret.md"), dir.path().join("secret.md")).unwrap();
    let lib = Library::new().overlay_dir(dir.path());
    let paths: Vec<&str> = lib.docs().iter().map(|d| d.path.as_str()).collect();
    assert_eq!(paths, ["page.md"]);
    let out = dir.path().join("table.rs");
    assert_eq!(embed(dir.path(), &out).unwrap(), 1);
}

#[test]
fn embed_writes_a_sorted_table_of_markdown_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("b")).unwrap();
    std::fs::write(dir.path().join("b/z.md"), "z").unwrap();
    std::fs::write(dir.path().join("a.md"), "a").unwrap();
    std::fs::write(dir.path().join("note.txt"), "t").unwrap();
    let out = dir.path().join("table.rs");
    assert_eq!(embed(dir.path(), &out).unwrap(), 2);
    let src = std::fs::read_to_string(&out).unwrap();
    assert!(src.starts_with("/// Every documentation file"), "{src}");
    let a = src.find("(\"a.md\", include_str!(").unwrap();
    let z = src.find("(\"b/z.md\", include_str!(").unwrap();
    assert!(a < z, "{src}");
    assert!(!src.contains("note.txt"));
}
