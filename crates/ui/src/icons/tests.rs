use egui::load::{ImagePoll, SizeHint};

use super::*;

const BOX: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="4" y="4" width="16" height="16"/><path d="M4 4 L20 20" stroke-width="2"/></svg>"##;
const KEEP: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M2 2h20" stroke="#123456" stroke-width="1"/><path d="M2 8h20" stroke='#654321' stroke-width='2'/><circle cx="12" cy="16" r="4" fill="#ff8800"/></svg>"##;

static ICONS: &[(&str, &[u8])] = &[("box", BOX), ("keep", KEEP)];

const LIGHT: SvgStyle = SvgStyle { current_color: "#ffffff", stroke_width: Some("1.75") };
const WORD: SvgStyle = SvgStyle { current_color: "white", stroke_width: Some("1.6") };

fn bytes(source: &ImageSource<'static>) -> (String, Arc<[u8]>) {
    match source {
        ImageSource::Bytes { uri, bytes: Bytes::Shared(arc) } => (uri.to_string(), Arc::clone(arc)),
        other => panic!("unexpected source {other:?}"),
    }
}

#[test]
fn styles_substitute_colour_and_stroke_width_only() {
    for (style, colour, width) in [(LIGHT, "#ffffff", "1.75"), (WORD, "white", "1.6")] {
        let set = SvgIconSet::new(ICONS, style, "bytes://test/icons/", "box");
        let (uri, svg) = bytes(&set.source("box"));
        assert_eq!(uri, "bytes://test/icons/box.svg");
        let text = std::str::from_utf8(&svg).unwrap();
        assert!(text.contains(&format!("stroke=\"{colour}\"")), "{text}");
        assert_eq!(text.matches(&format!("stroke-width=\"{width}\"")).count(), 2, "{text}");
        assert!(!text.contains("currentColor"));
        let (_, kept) = bytes(&set.source("keep"));
        assert_eq!(&kept[..], KEEP);
    }
    let unstyled = SvgIconSet::new(ICONS, SvgStyle { current_color: "#000", stroke_width: None }, "bytes://plain/", "box");
    let (_, svg) = bytes(&unstyled.source("box"));
    assert!(std::str::from_utf8(&svg).unwrap().contains("stroke-width=\"2\""));
}

#[test]
fn unknown_names_get_the_fallback_under_their_own_uri() {
    let set = SvgIconSet::new(ICONS, LIGHT, "bytes://test/icons/", "box");
    let (uri, missing) = bytes(&set.source("missing"));
    assert_eq!(uri, "bytes://test/icons/missing.svg");
    let (_, fallback) = bytes(&set.source("box"));
    assert!(Arc::ptr_eq(&missing, &fallback));
    let (_, again) = bytes(&set.source("missing"));
    assert!(Arc::ptr_eq(&missing, &again));
    assert!(!set.contains("missing"));
    assert_eq!(set.canonical_name("missing"), None);
    assert!(set.contains("box"));
    assert_eq!(set.canonical_name("box"), Some("box"));
    assert_eq!(set.icons.len(), 2, "lookups never add entries");

    let empty = SvgIconSet::new(&[], LIGHT, "bytes://none/", "absent");
    let (uri, svg) = bytes(&empty.source("anything"));
    assert_eq!((uri.as_str(), svg.len()), ("bytes://none/anything.svg", 0));
}

#[test]
fn duplicates_keep_the_last_and_bad_utf8_is_replaced() {
    static DUPLICATES: &[(&str, &[u8])] = &[("a", b"<svg>first</svg>"), ("a", b"<svg>second\xff</svg>")];
    let set = SvgIconSet::new(DUPLICATES, LIGHT, "bytes://dup/", "a");
    let (_, svg) = bytes(&set.source("a"));
    assert_eq!(std::str::from_utf8(&svg).unwrap(), "<svg>second\u{FFFD}</svg>");
}

#[test]
fn prepared_icons_rasterize_at_the_requested_size() {
    let ctx = egui::Context::default();
    egui_extras::install_image_loaders(&ctx);
    for (n, style) in [LIGHT, WORD].into_iter().enumerate() {
        let set = SvgIconSet::new(ICONS, style, if n == 0 { "bytes://raster/light/" } else { "bytes://raster/word/" }, "box");
        let (uri, svg) = bytes(&set.source("box"));
        ctx.include_bytes(uri.clone(), Bytes::Shared(svg));
        for px in [16, 24, 48] {
            let poll = ctx.try_load_image(&uri, SizeHint::Size { width: px, height: px, maintain_aspect_ratio: true }).unwrap();
            let ImagePoll::Ready { image } = poll else { panic!("{uri} not ready") };
            assert_eq!(image.size, [px as usize, px as usize]);
            assert!(image.pixels.iter().any(|p| p.a() > 0), "{uri} at {px}px is blank");
        }
    }
}
