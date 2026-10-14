//! A Windows 2000-style look for egui, to match the original client:
//! 3D-face gray, bevelled buttons, sunken white list boxes, etched group
//! boxes, navy selection and Tahoma 8pt.

use std::sync::Arc;

use eframe::egui::{
    self, pos2, vec2, Align2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Pos2, Rect,
    Response, RichText, Sense, Shadow, Shape, Stroke, TextStyle, Ui, Vec2, WidgetText,
};

pub const FACE: Color32 = Color32::from_rgb(0xd4, 0xd0, 0xc8);
pub const FACE_LIGHT: Color32 = Color32::from_rgb(0xe6, 0xe3, 0xde);
pub const HILIGHT: Color32 = Color32::WHITE;
pub const SHADOW: Color32 = Color32::from_rgb(0x80, 0x80, 0x80);
pub const DARK: Color32 = Color32::from_rgb(0x40, 0x40, 0x40);
pub const NAVY: Color32 = Color32::from_rgb(0x0a, 0x24, 0x6a);
pub const TITLE_END: Color32 = Color32::from_rgb(0xa6, 0xca, 0xf0);
pub const TEXT: Color32 = Color32::BLACK;
pub const DISABLED: Color32 = Color32::from_rgb(0x80, 0x80, 0x80);
pub const BAR_BLUE: Color32 = Color32::from_rgb(0x20, 0x30, 0xd8);
pub const BAR_YELLOW: Color32 = Color32::from_rgb(0xf0, 0xe8, 0x30);
pub const LINK: Color32 = Color32::from_rgb(0x00, 0x00, 0xc0);
pub const RED: Color32 = Color32::from_rgb(0xc0, 0x00, 0x00);

pub const ROW_H: f32 = 16.0;

fn bold_family() -> FontFamily {
    FontFamily::Name("bold".into())
}

/// Bold text (Tahoma Bold when available).
pub fn bold(text: impl Into<String>) -> RichText {
    RichText::new(text).family(bold_family()).color(TEXT)
}

fn load_font(candidates: &[&str]) -> Option<Vec<u8>> {
    candidates.iter().find_map(|p| std::fs::read(p).ok())
}

pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let regular = load_font(&[
        "C:\\Windows\\Fonts\\tahoma.ttf",
        "/System/Library/Fonts/Supplemental/Tahoma.ttf",
        "/Library/Fonts/Tahoma.ttf",
        "/usr/share/fonts/truetype/msttcorefonts/Tahoma.ttf",
        "/usr/share/fonts/truetype/msttcorefonts/tahoma.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ]);
    let bold = load_font(&[
        "C:\\Windows\\Fonts\\tahomabd.ttf",
        "/System/Library/Fonts/Supplemental/Tahoma Bold.ttf",
        "/Library/Fonts/Tahoma Bold.ttf",
        "/usr/share/fonts/truetype/msttcorefonts/Tahoma_Bold.ttf",
        "/usr/share/fonts/truetype/msttcorefonts/tahomabd.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
    ]);
    let fallbacks = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    if let Some(r) = regular {
        fonts.font_data.insert("classic".into(), Arc::new(FontData::from_owned(r)));
        fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "classic".into());
    }
    let mut bold_list = fallbacks;
    if let Some(b) = bold {
        fonts.font_data.insert("classic-bold".into(), Arc::new(FontData::from_owned(b)));
        bold_list.insert(0, "classic-bold".into());
    }
    fonts.families.insert(bold_family(), bold_list);
    ctx.set_fonts(fonts);

    ctx.style_mut(|s| {
        s.text_styles = [
            (TextStyle::Small, FontId::proportional(10.0)),
            (TextStyle::Body, FontId::proportional(12.0)),
            (TextStyle::Button, FontId::proportional(12.0)),
            (TextStyle::Heading, FontId::new(15.0, bold_family())),
            (TextStyle::Monospace, FontId::monospace(12.0)),
        ]
        .into();
        s.spacing.item_spacing = vec2(6.0, 4.0);
        s.spacing.button_padding = vec2(8.0, 2.0);
        s.spacing.interact_size = vec2(20.0, 20.0);
        s.spacing.window_margin = Margin::same(6);
        s.spacing.menu_margin = Margin::same(2);
        s.spacing.scroll = egui::style::ScrollStyle::solid();
        s.spacing.scroll.bar_width = 14.0;
        s.spacing.combo_height = 300.0;

        let v = &mut s.visuals;
        *v = egui::Visuals::light();
        v.override_text_color = Some(TEXT);
        v.panel_fill = FACE;
        v.window_fill = FACE;
        v.faint_bg_color = FACE_LIGHT;
        v.extreme_bg_color = Color32::WHITE;
        v.code_bg_color = Color32::WHITE;
        v.window_stroke = Stroke::new(1.0_f32, DARK);
        v.window_corner_radius = CornerRadius::ZERO;
        v.menu_corner_radius = CornerRadius::ZERO;
        v.window_shadow = Shadow::NONE;
        v.popup_shadow = Shadow::NONE;
        v.hyperlink_color = LINK;
        v.error_fg_color = RED;
        v.warn_fg_color = Color32::from_rgb(0x80, 0x60, 0x00);
        v.selection.bg_fill = NAVY;
        v.selection.stroke = Stroke::new(1.0_f32, Color32::WHITE);
        v.striped = false;
        v.slider_trailing_fill = true;
        let w = &mut v.widgets;
        for (wv, stroke) in [
            (&mut w.noninteractive, SHADOW),
            (&mut w.inactive, SHADOW),
            (&mut w.hovered, DARK),
            (&mut w.active, DARK),
            (&mut w.open, DARK),
        ] {
            wv.corner_radius = CornerRadius::ZERO;
            wv.bg_stroke = Stroke::new(1.0_f32, stroke);
            wv.fg_stroke = Stroke::new(1.0_f32, TEXT);
            wv.expansion = 0.0;
            wv.bg_fill = Color32::WHITE;
            wv.weak_bg_fill = FACE;
        }
        w.noninteractive.bg_fill = FACE;
        w.hovered.weak_bg_fill = FACE_LIGHT;
        w.active.weak_bg_fill = Color32::from_rgb(0xc4, 0xc0, 0xb8);
        w.open.weak_bg_fill = FACE_LIGHT;
    });
}

// ------------------------------------------------------------------ bevels

/// Classic two-pixel 3D border. Raised for buttons, sunken for fields.
pub fn bevel(p: &egui::Painter, r: Rect, raised: bool) {
    let r = r.shrink(0.5);
    let (outer_tl, inner_tl, inner_br, outer_br) = if raised {
        (HILIGHT, FACE_LIGHT, SHADOW, DARK)
    } else {
        (SHADOW, DARK, FACE_LIGHT, HILIGHT)
    };
    let line = |a: Pos2, b: Pos2, c: Color32| p.line_segment([a, b], Stroke::new(1.0_f32, c));
    line(r.left_bottom(), r.left_top(), outer_tl);
    line(r.left_top(), r.right_top(), outer_tl);
    line(r.right_top(), r.right_bottom(), outer_br);
    line(r.left_bottom(), r.right_bottom() + vec2(0.5, 0.0), outer_br);
    let i = r.shrink(1.0);
    line(i.left_bottom(), i.left_top(), inner_tl);
    line(i.left_top(), i.right_top(), inner_tl);
    line(i.right_top(), i.right_bottom(), inner_br);
    line(i.left_bottom(), i.right_bottom() + vec2(0.5, 0.0), inner_br);
}

/// One-pixel sunken edge used by status bar panels.
pub fn thin_sunken(p: &egui::Painter, r: Rect) {
    let r = r.shrink(0.5);
    p.line_segment([r.left_bottom(), r.left_top()], Stroke::new(1.0_f32, SHADOW));
    p.line_segment([r.left_top(), r.right_top()], Stroke::new(1.0_f32, SHADOW));
    p.line_segment([r.right_top(), r.right_bottom()], Stroke::new(1.0_f32, HILIGHT));
    p.line_segment([r.left_bottom(), r.right_bottom()], Stroke::new(1.0_f32, HILIGHT));
}

/// Etched line (group boxes, separators).
fn etched_rect(p: &egui::Painter, r: Rect) {
    p.rect_stroke(r.translate(vec2(1.0, 1.0)), CornerRadius::ZERO, Stroke::new(1.0_f32, HILIGHT), egui::StrokeKind::Inside);
    p.rect_stroke(r, CornerRadius::ZERO, Stroke::new(1.0_f32, SHADOW), egui::StrokeKind::Inside);
}

// ----------------------------------------------------------------- widgets

/// A push button. `width` of `None` sizes to the text.
pub fn button(ui: &mut Ui, text: impl Into<WidgetText>, width: Option<f32>) -> Response {
    button_ex(ui, text, width, true, false)
}

pub fn button_ex(ui: &mut Ui, text: impl Into<WidgetText>, width: Option<f32>, enabled: bool, pushed: bool) -> Response {
    let text: WidgetText = text.into();
    let galley = text.into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, TextStyle::Button);
    let w = width.unwrap_or(galley.size().x + 22.0).max(galley.size().x + 8.0);
    let h = 23.0;
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), sense);
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let down = pushed || (enabled && resp.is_pointer_button_down_on());
        p.rect_filled(rect, CornerRadius::ZERO, if pushed { FACE_LIGHT } else { FACE });
        bevel(p, rect, !down);
        let off = if down { vec2(1.0, 1.0) } else { Vec2::ZERO };
        let pos = rect.center() - galley.size() / 2.0 + off;
        if enabled {
            p.galley(pos, galley.clone(), TEXT);
        } else {
            p.galley(pos + vec2(1.0, 1.0), galley.clone(), HILIGHT);
            p.galley_with_override_text_color(pos, galley, DISABLED);
        }
        if resp.has_focus() {
            p.rect_stroke(
                rect.shrink(4.0),
                CornerRadius::ZERO,
                Stroke::new(1.0_f32, Color32::from_gray(90)),
                egui::StrokeKind::Inside,
            );
        }
    }
    resp
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Home,
    Chat,
    Library,
    Search,
    HotList,
    Transfer,
    Discover,
    Help,
}

/// Small 16x16 pictures for the tab buttons, drawn rather than taken from
/// a font so they look the same everywhere.
pub fn paint_icon(p: &egui::Painter, r: Rect, icon: Icon) {
    let s = Stroke::new(1.0_f32, Color32::from_gray(30));
    let at = |x: f32, y: f32| pos2(r.left() + x, r.top() + y);
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Rect::from_min_max(at(x0, y0), at(x1, y1));
    let inside = egui::StrokeKind::Inside;
    match icon {
        Icon::Home => {
            let roof = Color32::from_rgb(0xc0, 0x20, 0x20);
            p.add(Shape::convex_polygon(vec![at(8.0, 1.0), at(15.5, 8.0), at(0.5, 8.0)], roof, s));
            p.rect_filled(rect(2.5, 8.0, 13.5, 15.0), CornerRadius::ZERO, Color32::from_rgb(0x20, 0x48, 0xc8));
            p.rect_stroke(rect(2.5, 8.0, 13.5, 15.0), CornerRadius::ZERO, s, inside);
            p.rect_filled(rect(6.5, 10.0, 9.5, 15.0), CornerRadius::ZERO, Color32::from_rgb(0xf0, 0xd0, 0x60));
        }
        Icon::Chat => {
            let note = Color32::from_rgb(0xff, 0xf0, 0x90);
            p.rect_filled(rect(0.5, 1.5, 11.5, 9.5), CornerRadius::same(2), Color32::WHITE);
            p.rect_stroke(rect(0.5, 1.5, 11.5, 9.5), CornerRadius::same(2), s, inside);
            p.add(Shape::convex_polygon(vec![at(3.0, 9.0), at(6.0, 9.0), at(2.0, 12.5)], Color32::WHITE, s));
            p.rect_filled(rect(5.5, 6.5, 15.5, 13.5), CornerRadius::same(2), note);
            p.rect_stroke(rect(5.5, 6.5, 15.5, 13.5), CornerRadius::same(2), s, inside);
            p.add(Shape::convex_polygon(vec![at(11.0, 13.0), at(14.0, 13.0), at(14.5, 15.5)], note, s));
        }
        Icon::Library => {
            p.rect_filled(rect(0.5, 3.0, 7.0, 6.0), CornerRadius::ZERO, Color32::from_rgb(0xd8, 0xa8, 0x20));
            p.rect_filled(rect(0.5, 5.0, 15.5, 14.5), CornerRadius::ZERO, Color32::from_rgb(0xf0, 0xc8, 0x40));
            p.rect_stroke(rect(0.5, 5.0, 15.5, 14.5), CornerRadius::ZERO, s, inside);
            p.line_segment([at(0.5, 3.0), at(7.0, 3.0)], s);
        }
        Icon::Search => {
            p.circle_filled(at(6.5, 6.5), 4.8, Color32::from_rgb(0xc8, 0xe4, 0xff));
            p.circle_stroke(at(6.5, 6.5), 4.8, Stroke::new(1.6_f32, Color32::from_rgb(0x20, 0x40, 0xa0)));
            p.line_segment([at(10.0, 10.0), at(15.0, 15.0)], Stroke::new(3.0_f32, Color32::from_rgb(0x50, 0x30, 0x10)));
        }
        Icon::HotList => {
            p.rect_filled(rect(2.5, 1.5, 13.5, 15.0), CornerRadius::ZERO, Color32::from_rgb(0xff, 0xe8, 0x60));
            p.rect_stroke(rect(2.5, 1.5, 13.5, 15.0), CornerRadius::ZERO, s, inside);
            for y in [5.0, 8.0, 11.0] {
                p.line_segment([at(4.5, y), at(11.5, y)], Stroke::new(1.0_f32, Color32::from_rgb(0xc0, 0x40, 0x20)));
            }
            p.rect_filled(rect(4.0, 0.5, 12.0, 2.5), CornerRadius::ZERO, Color32::from_rgb(0xc0, 0x20, 0x20));
        }
        Icon::Transfer => {
            let green = Color32::from_rgb(0x10, 0xa0, 0x30);
            p.add(Shape::convex_polygon(vec![at(4.5, 1.0), at(8.5, 6.0), at(0.5, 6.0)], green, s));
            p.rect_filled(rect(3.0, 6.0, 6.0, 14.5), CornerRadius::ZERO, green);
            p.add(Shape::convex_polygon(vec![at(7.5, 10.0), at(15.5, 10.0), at(11.5, 15.0)], green, s));
            p.rect_filled(rect(10.0, 1.5, 13.0, 10.0), CornerRadius::ZERO, green);
        }
        Icon::Discover => {
            let red = Color32::from_rgb(0xc8, 0x10, 0x20);
            p.circle_filled(at(4.5, 12.5), 2.8, red);
            p.circle_filled(at(12.0, 11.0), 2.8, red);
            p.line_segment([at(7.0, 12.5), at(7.0, 2.0)], Stroke::new(1.6_f32, red));
            p.line_segment([at(14.5, 11.0), at(14.5, 0.8)], Stroke::new(1.6_f32, red));
            p.add(Shape::convex_polygon(vec![at(7.0, 2.0), at(14.5, 0.5), at(14.5, 3.5), at(7.0, 5.0)], red, Stroke::NONE));
        }
        Icon::Help => {
            p.circle_filled(at(8.0, 8.0), 7.0, Color32::from_rgb(0x20, 0x40, 0xb0));
            p.circle_stroke(at(8.0, 8.0), 7.0, s);
            p.text(at(8.0, 8.5), Align2::CENTER_CENTER, "?", FontId::new(12.0, bold_family()), Color32::WHITE);
        }
    }
}

/// The big tab buttons across the top (Home, Chat, Library...). The
/// selected one is drawn pushed in.
pub fn tab_button(ui: &mut Ui, icon: Icon, label: &str, selected: bool) -> Response {
    let font = FontId::proportional(12.0);
    let label_g = ui.painter().layout_no_wrap(label.to_owned(), font, TEXT);
    let icon_w = 16.0;
    let w = (icon_w + label_g.size().x + 26.0).max(86.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 25.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let down = selected || resp.is_pointer_button_down_on();
        let fill = if selected {
            FACE_LIGHT
        } else if resp.hovered() {
            Color32::from_rgb(0xdc, 0xd8, 0xd0)
        } else {
            FACE
        };
        p.rect_filled(rect, CornerRadius::ZERO, fill);
        bevel(p, rect, !down);
        let off = if down { vec2(1.0, 1.0) } else { Vec2::ZERO };
        let total = icon_w + 6.0 + label_g.size().x;
        let x = rect.center().x - total / 2.0;
        paint_icon(p, Rect::from_min_size(pos2(x, rect.center().y - 8.0) + off, vec2(16.0, 16.0)), icon);
        p.galley(pos2(x + icon_w + 6.0, rect.center().y - label_g.size().y / 2.0) + off, label_g, TEXT);
    }
    resp
}

/// Etched group box with a caption ("Search Fields").
pub fn group<R>(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    let title_g = ui.painter().layout_no_wrap(title.to_owned(), FontId::proportional(12.0), TEXT);
    let top = ui.cursor().top();
    ui.add_space(title_g.size().y / 2.0 + 2.0);
    let frame = egui::Frame::new().inner_margin(Margin { left: 8, right: 8, top: 10, bottom: 8 });
    let inner = frame.show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        add(ui)
    });
    let r = inner.response.rect;
    let box_rect = Rect::from_min_max(pos2(r.left(), top + title_g.size().y / 2.0), r.max - vec2(1.0, 1.0));
    let p = ui.painter();
    etched_rect(p, box_rect);
    let tpos = pos2(box_rect.left() + 8.0, top);
    p.rect_filled(Rect::from_min_size(tpos - vec2(2.0, 0.0), title_g.size() + vec2(4.0, 0.0)), CornerRadius::ZERO, FACE);
    p.galley(tpos, title_g, TEXT);
    inner.inner
}

/// Sunken box (list views, text areas) of a given size with `fill`.
pub fn sunken<R>(ui: &mut Ui, size: Vec2, fill: Color32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::ZERO, fill);
    let inner_rect = rect.shrink(2.0);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner_rect).layout(egui::Layout::top_down(egui::Align::Min)));
    child.set_clip_rect(inner_rect.intersect(ui.clip_rect()));
    let r = add(&mut child);
    bevel(ui.painter(), rect, false);
    r
}

/// Single-line text box with a sunken border.
pub fn text_field(ui: &mut Ui, text: &mut String, width: f32) -> Response {
    text_field_ex(ui, text, width, false, "")
}

pub fn text_field_ex(ui: &mut Ui, text: &mut String, width: f32, password: bool, hint: &str) -> Response {
    let bg = ui.painter().add(Shape::Noop);
    let resp = ui.add_sized(
        vec2(width, 20.0),
        egui::TextEdit::singleline(text)
            .frame(false)
            .password(password)
            .hint_text(RichText::new(hint).color(DISABLED))
            .desired_width(width - 8.0)
            .margin(vec2(5.0, 3.0))
            .text_color(TEXT),
    );
    let r = resp.rect;
    ui.painter().set(bg, Shape::rect_filled(r, CornerRadius::ZERO, Color32::WHITE));
    bevel(ui.painter(), r, false);
    resp
}

/// Progress bar as in the Transfer window: sunken, with "13 %" centered.
pub fn progress(ui: &mut Ui, frac: f32, color: Color32, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 14.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::ZERO, Color32::WHITE);
    let inner = rect.shrink(2.0);
    let frac = frac.clamp(0.0, 1.0);
    if frac > 0.0 {
        let filled = Rect::from_min_size(inner.min, vec2(inner.width() * frac, inner.height()));
        p.rect_filled(filled, CornerRadius::ZERO, color);
    }
    thin_sunken(p, rect);
    p.text(
        rect.center(),
        Align2::CENTER_CENTER,
        format!("{} %", (frac * 100.0).round() as u32),
        FontId::proportional(10.0),
        TEXT,
    );
}

/// Full-width recessed gray bar with centered text ("Returned 82 results.").
pub fn info_bar(ui: &mut Ui, text: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::ZERO, Color32::from_rgb(0xb8, 0xb4, 0xac));
    thin_sunken(p, rect);
    p.text(rect.center(), Align2::CENTER_CENTER, text, FontId::proportional(12.0), Color32::from_rgb(0x20, 0x20, 0x20));
}

/// A status bar panel.
pub fn status_panel(ui: &mut Ui, text: &str, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 18.0), Sense::hover());
    thin_sunken(ui.painter(), rect);
    let clip = rect.shrink(2.0);
    ui.painter().with_clip_rect(clip).text(
        pos2(rect.left() + 4.0, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        FontId::proportional(12.0),
        TEXT,
    );
}

/// Column header cell: raised button look filling the cell.
pub fn header_cell(ui: &mut Ui, text: &str) -> Response {
    let rect = ui.max_rect().expand2(vec2(3.0, 1.0));
    let resp = ui.interact(rect, ui.id().with(text), Sense::click());
    let p = ui.painter().with_clip_rect(rect);
    p.rect_filled(rect, CornerRadius::ZERO, FACE);
    bevel(&p, rect, !resp.is_pointer_button_down_on());
    p.text(pos2(rect.left() + 5.0, rect.center().y), Align2::LEFT_CENTER, text, FontId::proportional(12.0), TEXT);
    resp
}

/// Table cell text, white on navy when the row is selected.
pub fn cell(ui: &mut Ui, text: impl Into<String>, selected: bool) -> Response {
    let color = if selected { Color32::WHITE } else { TEXT };
    ui.add(egui::Label::new(RichText::new(text.into()).color(color)).truncate().selectable(false))
}

/// Navy title bar for dialogs, with a close box. Returns true if closed.
pub fn title_bar(ui: &mut Ui, title: &str) -> bool {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    let p = ui.painter();
    // horizontal gradient navy -> light blue like Win2000
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), NAVY);
    mesh.colored_vertex(rect.right_top(), TITLE_END);
    mesh.colored_vertex(rect.right_bottom(), TITLE_END);
    mesh.colored_vertex(rect.left_bottom(), NAVY);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    p.add(Shape::mesh(mesh));
    p.text(
        pos2(rect.left() + 6.0, rect.center().y),
        Align2::LEFT_CENTER,
        title,
        FontId::new(12.0, bold_family()),
        Color32::WHITE,
    );
    let close = Rect::from_center_size(pos2(rect.right() - 11.0, rect.center().y), vec2(16.0, 14.0));
    let resp = ui.interact(close, ui.id().with(("close", title)), Sense::click());
    p.rect_filled(close, CornerRadius::ZERO, FACE);
    bevel(p, close, !resp.is_pointer_button_down_on());
    let c = close.center();
    for (a, b) in [(vec2(-3.5, -3.0), vec2(3.5, 3.0)), (vec2(-3.5, 3.0), vec2(3.5, -3.0))] {
        p.line_segment([c + a, c + b], Stroke::new(1.5_f32, TEXT));
    }
    resp.clicked()
}

/// A dialog window with a classic frame and title bar. `open` is cleared
/// when the close box is clicked.
pub fn dialog<R>(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    open: &mut bool,
    width: f32,
    add: impl FnOnce(&mut Ui) -> R,
) -> Option<R> {
    let mut close = false;
    let r = egui::Window::new(id)
        .id(egui::Id::new(id))
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .default_width(width)
        .pivot(Align2::CENTER_CENTER)
        .default_pos(ctx.screen_rect().center())
        .frame(egui::Frame::new().fill(FACE).inner_margin(Margin::same(3)).stroke(Stroke::NONE))
        .show(ctx, |ui| {
            ui.set_width(width);
            close = title_bar(ui, title);
            ui.add_space(4.0);
            let r = egui::Frame::new().inner_margin(Margin::symmetric(6, 4)).show(ui, add).inner;
            let outer = ui.min_rect().expand(3.0);
            bevel(ui.painter(), outer, true);
            r
        });
    if close {
        *open = false;
    }
    r.and_then(|r| r.inner)
}

/// Our stand-in for the logo: headphones and a "napster" wordmark. (The
/// original artwork isn't bundled.)
pub fn wordmark(ui: &mut Ui, height: f32, dark_bg: bool) {
    let w = height * 4.6;
    let (rect, _) = ui.allocate_exact_size(vec2(w, height), Sense::hover());
    let p = ui.painter();
    let c = pos2(rect.left() + height * 0.5, rect.center().y + height * 0.04);
    let r = height * 0.36;
    let blue = Color32::from_rgb(0x1f, 0x3c, 0xa0);
    // headband
    let pts: Vec<Pos2> = (0..=24)
        .map(|i| {
            let a = std::f32::consts::PI * (1.0 + i as f32 / 24.0);
            c + vec2(a.cos() * r, a.sin() * r)
        })
        .collect();
    p.add(Shape::line(pts, Stroke::new(height * 0.09, blue)));
    // ear cups
    for dx in [-r, r] {
        let cup = Rect::from_center_size(c + vec2(dx, height * 0.08), vec2(height * 0.18, height * 0.32));
        p.rect_filled(cup, CornerRadius::same((height * 0.06) as u8), blue);
    }
    let text_color = if dark_bg { Color32::from_rgb(0xb0, 0xb0, 0xb0) } else { Color32::from_rgb(0x55, 0x55, 0x58) };
    p.text(
        pos2(rect.left() + height * 1.05, rect.center().y),
        Align2::LEFT_CENTER,
        "napster",
        FontId::new(height * 0.78, bold_family()),
        text_color,
    );
}

/// "3,665,975"
pub fn commas(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn commas() {
        assert_eq!(super::commas(0), "0");
        assert_eq!(super::commas(999), "999");
        assert_eq!(super::commas(3665975), "3,665,975");
    }
}
