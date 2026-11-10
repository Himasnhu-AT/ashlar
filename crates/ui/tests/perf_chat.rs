//! Chat transcript frame cost: a frame draws only the visible messages, so it stays flat as the
//! message count grows. Run in release:
//! `cargo test -p ashlar-ui --release --test perf_chat -- --ignored --nocapture --test-threads=1`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Instant;

use ashlar_ui::chat::{ChatMessage, ChatRole, Message, Transcript};

const FRAMES: usize = 60;

fn messages(n: u64) -> Vec<ChatMessage> {
    (0..n)
        .map(|i| ChatMessage {
            id: i,
            role: if i.is_multiple_of(2) { ChatRole::User } else { ChatRole::Assistant },
            text: format!("Message **{i}** with `code` and a [link](https://example.com).\n\n- one\n- two\n\nA closing paragraph."),
            streaming: false,
            revision: 0,
        })
        .collect()
}

fn frame(ctx: &egui::Context, t: f64, msgs: &[ChatMessage], revision: u64) -> f64 {
    let input = egui::RawInput {
        time: Some(t),
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 700.0))),
        ..Default::default()
    };
    let mut ms = 0.0;
    ctx.run_ui(input, |ui| {
        let start = Instant::now();
        Transcript::new("chat", msgs, revision).show(ui, |ui, m| {
            let id = format!("chat.msg.{}", m.id);
            Message::new(&id, m.role, &m.text).streaming(m.streaming).show(ui);
        });
        ms = start.elapsed().as_secs_f64() * 1e3;
    })
    .textures_delta
    .clear();
    ms
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(f64::NAN)
}

/// Median frame time (ms) while the last message streams in, one token per frame.
fn streaming_frame(n: u64) -> f64 {
    let ctx = egui::Context::default();
    let mut msgs = messages(n);
    let mut revision = 1;
    for k in 0..5 {
        frame(&ctx, k as f64 / 60.0, &msgs, revision);
    }
    let mut times = Vec::with_capacity(FRAMES);
    for k in 0..FRAMES {
        if let Some(last) = msgs.last_mut() {
            last.text.push_str(" token");
            last.revision += 1;
            last.streaming = true;
        }
        revision += 1;
        times.push(frame(&ctx, (k + 5) as f64 / 60.0, &msgs, revision));
    }
    median(times)
}

#[test]
#[ignore]
fn perf_transcript_flat_with_message_count() {
    let small = streaming_frame(1_000);
    let large = streaming_frame(10_000);
    println!("bench: transcript_1k_messages_frame {small:.3} ms");
    println!("bench: transcript_10k_messages_frame {large:.3} ms");
    assert!(large < small * 3.0 + 0.5, "10k messages cost {large:.3} ms against {small:.3} ms for 1k");
}
