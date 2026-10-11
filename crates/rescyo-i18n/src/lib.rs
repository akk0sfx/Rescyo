//! Localized user-facing strings (Project Fluent). Machine output is never localized.
//!
//! Locales are compiled into the binary so the portable executable works from any medium.
#![forbid(unsafe_code)]

use std::borrow::Cow;

use fluent_bundle::{FluentArgs, FluentBundle, FluentResource, FluentValue};
use unic_langid::LanguageIdentifier;

/// Supported interface languages, as (tag, Fluent source).
pub const LOCALES: [(&str, &str); 5] = [
    ("en", include_str!("../locales/en.ftl")),
    ("ru", include_str!("../locales/ru.ftl")),
    ("zh-CN", include_str!("../locales/zh-CN.ftl")),
    ("ja", include_str!("../locales/ja.ftl")),
    ("ko", include_str!("../locales/ko.ftl")),
];

/// Picks the best supported locale for a BCP 47 tag (`ru-RU` → `ru`, `zh-Hans-CN` → `zh-CN`).
pub fn negotiate(tag: &str) -> &'static str {
    let t = tag.trim().replace('_', "-").to_ascii_lowercase();
    let primary = t.split(['-', '.', '@']).next().unwrap_or("");
    match primary {
        "ru" => "ru",
        "zh" => "zh-CN",
        "ja" => "ja",
        "ko" => "ko",
        _ => "en",
    }
}

/// Formats messages for one language, falling back to English for anything missing.
pub struct Localizer {
    lang: &'static str,
    bundle: FluentBundle<FluentResource>,
    fallback: FluentBundle<FluentResource>,
}

impl std::fmt::Debug for Localizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Localizer")
            .field("lang", &self.lang)
            .finish_non_exhaustive()
    }
}

fn bundle(tag: &str) -> FluentBundle<FluentResource> {
    let src = LOCALES
        .iter()
        .find(|(t, _)| *t == tag)
        .map_or(LOCALES[0].1, |(_, s)| s);
    let id: LanguageIdentifier = tag.parse().unwrap_or_default();
    let mut b = FluentBundle::new(vec![id]);
    // Terminals do not need Unicode isolation marks around placeables.
    b.set_use_isolating(false);
    let res = match FluentResource::try_new(src.to_owned()) {
        Ok(r) => r,
        Err((r, _)) => r,
    };
    let _ = b.add_resource(res);
    b
}

impl Localizer {
    /// `lang` from `--lang`; otherwise the OS locale; otherwise English.
    pub fn new(lang: Option<&str>) -> Self {
        let tag = lang
            .map(str::to_owned)
            .or_else(sys_locale::get_locale)
            .unwrap_or_else(|| "en".into());
        let lang = negotiate(&tag);
        Self {
            lang,
            bundle: bundle(lang),
            fallback: bundle("en"),
        }
    }

    pub fn lang(&self) -> &'static str {
        self.lang
    }

    /// Formats message `id` with `args` (name, value) pairs. Unknown ids return the id itself,
    /// so a missing translation is visible rather than silent.
    pub fn t(&self, id: &str, args: &[(&str, Arg<'_>)]) -> String {
        let mut fa = FluentArgs::new();
        for (k, v) in args {
            match v {
                Arg::Str(s) => fa.set(*k, FluentValue::from(s.as_ref())),
                Arg::Num(n) => fa.set(*k, FluentValue::from(*n)),
            }
        }
        for b in [&self.bundle, &self.fallback] {
            if let Some(msg) = b.get_message(id)
                && let Some(p) = msg.value()
            {
                let mut errors = Vec::new();
                return b.format_pattern(p, Some(&fa), &mut errors).into_owned();
            }
        }
        id.to_owned()
    }
}

/// A message argument.
#[derive(Debug, Clone)]
pub enum Arg<'a> {
    Str(Cow<'a, str>),
    Num(i64),
}

impl<'a> From<&'a str> for Arg<'a> {
    fn from(s: &'a str) -> Self {
        Self::Str(Cow::Borrowed(s))
    }
}

impl From<String> for Arg<'_> {
    fn from(s: String) -> Self {
        Self::Str(Cow::Owned(s))
    }
}

impl From<u64> for Arg<'_> {
    fn from(n: u64) -> Self {
        Self::Num(i64::try_from(n).unwrap_or(i64::MAX))
    }
}

impl From<i64> for Arg<'_> {
    fn from(n: i64) -> Self {
        Self::Num(n)
    }
}

/// Sizes in binary units with one decimal (`4.2 MiB`); units are the same in every language.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS.get(u).unwrap_or(&"B"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluent_bundle::FluentResource;
    use std::collections::BTreeSet;

    fn ids(src: &str) -> BTreeSet<String> {
        let r =
            FluentResource::try_new(src.to_owned()).unwrap_or_else(|(_, e)| panic!("parse errors: {e:?}"));
        r.entries()
            .filter_map(|e| match e {
                fluent_syntax::ast::Entry::Message(m) => Some(m.id.name.to_owned()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn every_locale_parses_and_has_every_message() {
        let en = ids(LOCALES[0].1);
        for (tag, src) in LOCALES {
            let got = ids(src);
            let missing: Vec<_> = en.difference(&got).collect();
            let extra: Vec<_> = got.difference(&en).collect();
            assert!(
                missing.is_empty() && extra.is_empty(),
                "{tag}: missing {missing:?}, extra {extra:?}"
            );
        }
    }

    #[test]
    fn every_message_formats_without_errors() {
        let args: Vec<(&str, Arg<'_>)> = [
            "source", "path", "session", "target", "report", "reason", "devices", "fs", "offset", "log",
            "size", "needed", "free", "limit",
        ]
        .iter()
        .map(|k| (*k, Arg::from("x")))
        .chain(
            [
                "count", "files", "dirs", "deleted", "lost", "id", "cluster", "score", "index", "folders",
                "seeds", "offgrid",
            ]
            .iter()
            .map(|k| (*k, Arg::from(3u64))),
        )
        .collect();
        for (tag, _) in LOCALES {
            let b = bundle(tag);
            for id in ids(LOCALES[0].1) {
                let mut fa = FluentArgs::new();
                for (k, v) in &args {
                    match v {
                        Arg::Str(s) => fa.set(*k, FluentValue::from(s.as_ref())),
                        Arg::Num(n) => fa.set(*k, FluentValue::from(*n)),
                    }
                }
                let msg = b.get_message(&id).unwrap_or_else(|| panic!("{tag}: {id}"));
                let mut errors = Vec::new();
                let _ = b.format_pattern(msg.value().unwrap(), Some(&fa), &mut errors);
                assert!(errors.is_empty(), "{tag}: {id}: {errors:?}");
            }
        }
    }

    #[test]
    fn russian_plurals_and_fallback() {
        let ru = Localizer::new(Some("ru_RU.UTF-8"));
        assert_eq!(ru.lang(), "ru");
        assert_eq!(ru.t("ls-total", &[("count", 1u64.into())]), "1 запись");
        assert_eq!(ru.t("ls-total", &[("count", 3u64.into())]), "3 записи");
        assert_eq!(ru.t("ls-total", &[("count", 5u64.into())]), "5 записей");
        assert_eq!(ru.t("ls-total", &[("count", 21u64.into())]), "21 запись");
        let en = Localizer::new(Some("de-DE"));
        assert_eq!(en.lang(), "en");
        assert_eq!(en.t("ls-total", &[("count", 1u64.into())]), "1 entry");
        assert_eq!(en.t("no-such-message", &[]), "no-such-message");
        assert_eq!(negotiate("zh-Hans-CN"), "zh-CN");
        assert_eq!(negotiate("ja_JP"), "ja");
    }

    #[test]
    fn sizes() {
        assert_eq!(size(512), "512 B");
        assert_eq!(size(4_404_019), "4.2 MiB");
        assert_eq!(size(31_457_280_000), "29.3 GiB");
    }
}
