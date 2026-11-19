use rune::{Cx, El, TextStyle, col, row, spacer, text};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit, Progress};

use crate::{Lane, Leg, Side, State};

const HEADLINE: TextStyle = TextStyle::new(20.0, 600.0);

pub fn view(s: &State, cx: &mut Cx<State>) -> El {
    let k = rune_theme_opencode::kit(cx.system_dark());
    let refresh = Button::new("Refresh")
        .variant(ButtonVariant::Ghost)
        .control_size(ControlSize::Regular)
        .disabled(s.busy())
        .on_press(|s: &mut State| s.refresh())
        .build(k)
        .key("refresh");
    let header = row()
        .center_y()
        .child(text("Ricochet").font(HEADLINE).color(k.c.text_strong))
        .child(spacer())
        .child(refresh);
    let root = crate::worker::library().shown_root();
    let library = match s.library {
        Some((files, bytes)) => format!("{root} · {} files · {}", thousands(files), size(bytes)),
        None => root,
    };
    let running = [&s.camera, &s.phone].iter().any(|l| matches!(l.leg, Leg::Running { .. }));
    let pending = [&s.camera, &s.phone].iter().any(|l| matches!(l.leg, Leg::Ready { files: 1.., .. }));
    let action = if s.busy() && running {
        Some(Button::new("Stop").variant(ButtonVariant::Outline).on_press(|s: &mut State| s.cancel()))
    } else if !s.busy() && pending {
        Some(Button::new("Sync").variant(ButtonVariant::Submit).on_press(|s: &mut State| s.sync()))
    } else {
        None
    };
    let mut footer = row()
        .center_y()
        .gap(12.0)
        .child(text(library).key("library").font(k.ty.text_small).color(k.c.text_weak).truncate().grow(1.0).min_w(0.0));
    if let Some(action) = action {
        footer = footer.child(action.build(k).key("action"));
    }
    col()
        .w_full()
        .h_full()
        .padding(20.0)
        .gap(12.0)
        .fill(k.c.bg_base)
        .child(header)
        .child(lane(k, Side::Camera, s.lane(Side::Camera), s.busy()))
        .child(lane(k, Side::Phone, s.lane(Side::Phone), s.busy()))
        .child(spacer())
        .child(footer)
}

fn lane(k: &Kit, side: Side, lane: &Lane, busy: bool) -> El {
    let (key, route, device_kind) = match side {
        Side::Camera => ("camera", "Camera → Laptop", "camera"),
        Side::Phone => ("phone", "Laptop → iPhone", "iPhone"),
    };
    let device = match &lane.leg {
        Leg::Ready { device, .. } | Leg::Running { device, .. } | Leg::Listing { device, .. } => {
            shown_name(device, device_kind)
        }
        Leg::Absent => format!("No {device_kind}"),
        Leg::Checking | Leg::Failed { .. } => String::new(),
    };
    let plural = |n: usize| if n == 1 { "file" } else { "files" };
    let (headline, detail) = match &lane.leg {
        Leg::Checking => ("Checking…".to_owned(), String::new()),
        Leg::Listing { done, total, .. } => ("Checking…".into(), format!("{} of {} files", thousands(*done), thousands(*total))),
        Leg::Absent => match side {
            Side::Camera => ("Connect your camera".into(), "Plug it in over USB and switch it on.".into()),
            Side::Phone => ("Connect your iPhone".into(), "Plug it in, unlock it, and trust this computer.".into()),
        },
        Leg::Ready { files: 0, .. } => ("Up to date".into(), lane.last.clone().unwrap_or_default()),
        Leg::Ready { files, bytes, raw, jpg, .. } => (
            match side {
                Side::Camera => format!("{} new {}", thousands(*files), plural(*files)),
                Side::Phone => format!("{} {} to send", thousands(*files), plural(*files)),
            },
            format!("{} · {raw} RAW, {jpg} JPG", size(*bytes)),
        ),
        Leg::Running { index, files, name, done, bytes, .. } => (
            format!("{} {} of {}", if side == Side::Camera { "Importing" } else { "Sending" }, index + 1, files),
            format!("{name} · {} of {}", size(*done), size(*bytes)),
        ),
        Leg::Failed { error } => ("Couldn't finish".into(), error.clone()),
    };
    let top = row()
        .center_y()
        .gap(8.0)
        .child(text(route).font(k.ty.text_small_medium).color(k.c.text_weak))
        .child(spacer())
        .child(text(device).key("device").font(k.ty.text_small).color(k.c.text_weak).truncate());
    let detail_color = if matches!(lane.leg, Leg::Failed { .. }) { k.c.danger.fg } else { k.c.text_weak };
    let body = col()
        .gap(2.0)
        .grow(1.0)
        .min_w(0.0)
        .child(text(headline).key("headline").font(k.ty.body_medium).color(k.c.text_strong))
        .child(text(detail).key("detail").font(k.ty.text_small).color(detail_color).truncate_middle());
    let mut card = col()
        .key(key)
        .w_full()
        .padding(16.0)
        .gap(10.0)
        .radius(k.radius.md)
        .fill(k.c.surface_raised_base)
        .border(1.0, k.c.border_weak)
        .child(top)
        .child(body);
    let fraction = match lane.leg {
        Leg::Running { done, bytes, .. } => Some(done as f32 / bytes.max(1) as f32),
        Leg::Listing { done, total, .. } => Some(done as f32 / total.max(1) as f32),
        _ => None,
    };
    if let Some(fraction) = fraction {
        card = card.child(Progress::new(Some(fraction)).build(k).key("progress"));
    }
    // Copied before and deleted since: left out, with a way to copy them again.
    if let Leg::Ready { deleted: deleted @ 1.., .. } = lane.leg {
        let (what, again) = match side {
            Side::Camera => (format!("{} deleted from the library, still on the camera", thousands(deleted)), "Import again"),
            Side::Phone => (format!("{} deleted from the iPhone", thousands(deleted)), "Send again"),
        };
        let restore = Button::new(again)
            .variant(ButtonVariant::Ghost)
            .control_size(ControlSize::Small)
            .disabled(busy)
            .on_press(move |s: &mut State| s.restore(side))
            .build(k);
        card = card.child(
            row()
                .center_y()
                .gap(8.0)
                .child(text(what).key("deleted").font(k.ty.text_small).color(k.c.text_weak).truncate().grow(1.0).min_w(0.0))
                .child(restore.key("restore")),
        );
    }
    card
}

/// A device's name with only letters, digits, spaces and common punctuation. Emoji go because
/// Rune doesn't draw Linux's bitmap emoji font yet: a phone named "👻" showed an empty box.
/// A name with nothing left is its kind ("iPhone").
fn shown_name(name: &str, kind: &str) -> String {
    let kept: String = name
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || "'’-_.,()&+".contains(*c))
        .collect();
    let kept = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    if kept.is_empty() { kind.to_owned() } else { kept }
}

fn size(bytes: u64) -> String {
    match bytes {
        b if b >= 1_000_000_000 => format!("{:.2} GB", b as f64 / 1e9),
        b if b >= 1_000_000 => format!("{:.0} MB", b as f64 / 1e6),
        b => format!("{:.0} KB", b as f64 / 1e3),
    }
}

fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::shown_name;

    #[test]
    fn names_lose_emoji_and_fall_back_to_the_kind() {
        assert_eq!(shown_name("👻", "iPhone"), "iPhone");
        assert_eq!(shown_name("Naresh’s iPhone 🚀", "iPhone"), "Naresh’s iPhone");
        assert_eq!(shown_name("RICOH GR IV", "camera"), "RICOH GR IV");
    }
}
