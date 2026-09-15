#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Writing the user's own pages: paths, frontmatter, built-in pages, links, atomic replace.

use ashlar_docs::write::{MAX_WRITE_BYTES, append, check_path, delete, save};
use ashlar_docs::{Library, Source};

const STYLE: &str = "---\ntitle: Editing style\ndescription: Read before cutting\ntags: [memory]\n---\nHard cuts, no wipes.\n";

fn built_in() -> Library {
    Library::from_files([
        ("skills/review.md", "---\ntitle: Review\ndescription: Before reporting\n---\nLook.\n"),
        ("memory/_category.md", "---\ntitle: Memory\ndescription: The person's notes\n---\nRead first.\n"),
    ])
}

#[test]
fn paths_stay_inside_the_folder() {
    for bad in [
        "",
        "/etc/x.md",
        "~/x.md",
        "../x.md",
        "memory/../../x.md",
        "memory/./x.md",
        "memory//x.md",
        ".hidden.md",
        "memory/.x.md",
        "memory/x.txt",
        "memory\\x.md",
        "C:/x.md",
        "memory/a b.md",
        "a/b/c/d/e/f/g/h/i/j.md",
    ] {
        assert!(check_path(bad).is_err(), "{bad:?} should be refused");
    }
    assert_eq!(check_path(" docs://memory/edits/launch-film.md ").unwrap().0, "memory/edits/launch-film.md");
}

#[test]
fn save_append_delete() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("agent-docs");
    let lib = built_in();
    let w = save(&dir, &lib, "memory/style.md", STYLE, false).unwrap();
    assert!(w.created && !w.replaces_built_in && w.file == dir.join("memory/style.md"));
    let text = std::fs::read_to_string(&w.file).unwrap();
    assert!(text.contains("\nupdated: ") && text.ends_with("Hard cuts, no wipes.\n"), "{text}");

    // the overlay reads it like any other page
    let over = lib.clone().overlay_dir(&dir);
    let doc = over.docs().iter().find(|d| d.path == "memory/style.md").unwrap();
    assert_eq!((doc.title.as_str(), doc.source), ("Editing style", Source::User));
    assert_eq!(over.list(Some("memory"), None, true).len(), 1);

    let w = append(&dir, "memory/style.md", "- Captions: two words at a time.").unwrap();
    assert!(!w.created);
    let text = std::fs::read_to_string(&w.file).unwrap();
    assert!(text.ends_with("Hard cuts, no wipes.\n\n- Captions: two words at a time.\n"), "{text}");
    assert_eq!(text.matches("updated:").count(), 1);
    assert!(append(&dir, "memory/new.md", "x").unwrap_err().contains("save it first"));
    assert!(append(&dir, "memory/style.md", "  \n").is_err());

    // replacing the user's own page
    let w = save(&dir, &lib, "memory/style.md", "---\ntitle: Style\ndescription: Short\n---\nNew.\n", false).unwrap();
    assert!(!w.created);
    assert!(std::fs::read_to_string(&w.file).unwrap().ends_with("New.\n"));
    // no leftovers from the atomic writes
    let names: Vec<String> = std::fs::read_dir(dir.join("memory")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["style.md"]);

    delete(&dir, &lib, "memory/style.md").unwrap();
    assert!(!dir.join("memory/style.md").exists());
    assert!(delete(&dir, &lib, "memory/style.md").unwrap_err().contains("no such page"));
    assert!(delete(&dir, &lib, "skills/review.md").unwrap_err().contains("built-in"));
}

#[test]
fn frontmatter_size_and_built_in_pages() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let lib = built_in();
    assert!(save(dir, &lib, "memory/x.md", "no frontmatter", false).unwrap_err().contains("title"));
    assert!(save(dir, &lib, "memory/x.md", "---\ntitle: X\n---\nbody", false).unwrap_err().contains("description"));
    let big = format!("---\ntitle: X\ndescription: Y\n---\n{}", "a".repeat(MAX_WRITE_BYTES));
    assert!(save(dir, &lib, "memory/x.md", &big, false).unwrap_err().contains("at most"));

    // a built-in page or category is replaced only when asked
    let mine = "---\ntitle: My review\ndescription: Mine\n---\nMine.\n";
    assert!(save(dir, &lib, "skills/review.md", mine, false).unwrap_err().contains("built-in"));
    assert!(save(dir, &lib, "memory/_category.md", mine, false).unwrap_err().contains("built-in"));
    let w = save(dir, &lib, "skills/review.md", mine, true).unwrap();
    assert!(w.replaces_built_in);
    let over = lib.clone().overlay_dir(dir);
    assert_eq!(over.docs().iter().find(|d| d.path == "skills/review.md").unwrap().title, "My review");
    // deleting the user's copy brings the built-in page back
    assert!(delete(dir, &lib, "skills/review.md").unwrap().replaces_built_in);
    assert_eq!(lib.clone().overlay_dir(dir).docs().iter().find(|d| d.path == "skills/review.md").unwrap().title, "Review");
    // a new category of the user's own needs no permission
    save(dir, &lib, "notes/_category.md", mine, false).unwrap();
}

#[cfg(unix)]
#[test]
fn symbolic_links_are_not_followed() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("agent-docs");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, dir.join("memory")).unwrap();
    let lib = built_in();
    assert!(save(&dir, &lib, "memory/style.md", STYLE, false).unwrap_err().contains("symbolic link"));
    std::fs::create_dir_all(dir.join("real")).unwrap();
    std::os::unix::fs::symlink(outside.join("target.md"), dir.join("real/page.md")).unwrap();
    assert!(save(&dir, &lib, "real/page.md", STYLE, false).is_err());
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0, "nothing was written outside");
}

#[test]
fn built_in_only_drops_the_users_pages() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = built_in();
    save(tmp.path(), &lib, "memory/style.md", STYLE, false).unwrap();
    let over = lib.clone().overlay_dir(tmp.path());
    assert_eq!(over.docs().len(), 2);
    assert_eq!(over.built_in_only(), lib);
}
