//! Fills the window's `L` global from the Fluent catalogue of the interface language.

use rescyo_i18n::{Arg, Localizer};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::{L, MainWindow};

fn list(l: &Localizer, ids: &[&str]) -> ModelRc<SharedString> {
    let v: Vec<SharedString> = ids.iter().map(|id| SharedString::from(l.t(id, &[]))).collect();
    ModelRc::from(std::rc::Rc::new(VecModel::from(v)))
}

pub fn fill(ui: &MainWindow, l: &Localizer) {
    let g = ui.global::<L>();
    let t = |id: &str| SharedString::from(l.t(id, &[]));
    g.set_sources(t("gui-sources"));
    g.set_images(t("gui-images"));
    g.set_sessions(t("gui-sessions"));
    g.set_steps(list(
        l,
        &[
            "tui-step-source",
            "tui-step-scan",
            "tui-step-browse",
            "tui-step-recover",
        ],
    ));
    g.set_columns(list(
        l,
        &[
            "tui-col-name",
            "tui-col-size",
            "tui-col-modified",
            "tui-col-assembly",
            "tui-col-integrity",
        ],
    ));
    g.set_integrity(list(
        l,
        &["tui-intact", "tui-partial", "tui-damaged", "tui-unverified"],
    ));
    g.set_recover_to(t("tui-recover-to"));
    g.set_no_target(t("gui-no-target"));
    g.set_image_first(t("gui-image-first"));
    g.set_empty(t("tui-empty"));
    g.set_empty_title(t("gui-empty-title"));
    g.set_empty_hint(t("gui-empty-hint"));
    g.set_open_image(t("gui-open-image"));
    g.set_open_session(t("gui-open-session"));
    g.set_deep(t("gui-deep"));
    g.set_carve(t("gui-carve"));
    g.set_session_file(t("gui-session-file"));
    g.set_change(t("gui-change"));
    g.set_scan(t("gui-scan"));
    g.set_stop(t("gui-stop"));
    g.set_ok(t("gui-ok"));
    g.set_cancel(t("gui-cancel"));
    set_recover_button(ui, l, 0);
}

pub fn set_recover_button(ui: &MainWindow, l: &Localizer, count: u64) {
    ui.global::<L>().set_recover_button(SharedString::from(
        l.t("gui-recover-button", &[("count", Arg::from(count))]),
    ));
}
