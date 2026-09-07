//! Fluent-backed GUI localization with an explicit, deterministic fallback chain.

use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};
use unic_langid::{LanguageIdentifier, langid};

pub const LANGUAGE_ENV: &str = "HAUCET_GUI_LANGUAGE";

const EN_US: &str = include_str!("../i18n/en-US.ftl");
const ZH_CN: &str = include_str!("../i18n/zh-CN.ftl");
const RU_RU: &str = include_str!("../i18n/ru-RU.ftl");

/// Languages shipped with the GUI, in the requested fallback order.
pub const FALLBACK_ORDER: [Language; 3] = [Language::English, Language::Chinese, Language::Russian];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Language {
    #[default]
    English,
    Chinese,
    Russian,
}

impl Language {
    pub const ALL: [Self; 3] = [Self::English, Self::Chinese, Self::Russian];

    pub const fn native_name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Chinese => "中文",
            Self::Russian => "Русский",
        }
    }

    pub const fn tag(self) -> &'static str {
        match self {
            Self::English => "en-US",
            Self::Chinese => "zh-CN",
            Self::Russian => "ru-RU",
        }
    }

    pub fn from_tag(tag: &str) -> Option<Self> {
        match tag.trim().to_ascii_lowercase().as_str() {
            "en" | "en-us" => Some(Self::English),
            "zh" | "zh-cn" | "zh-hans" => Some(Self::Chinese),
            "ru" | "ru-ru" => Some(Self::Russian),
            _ => None,
        }
    }

    const fn index(self) -> u8 {
        match self {
            Self::English => 0,
            Self::Chinese => 1,
            Self::Russian => 2,
        }
    }

    const fn from_index(value: u8) -> Self {
        match value {
            1 => Self::Chinese,
            2 => Self::Russian,
            _ => Self::English,
        }
    }
}

static CURRENT_LANGUAGE: AtomicU8 = AtomicU8::new(Language::English.index());
static LOCALIZER: OnceLock<Localizer> = OnceLock::new();

pub fn set_language(language: Language) {
    CURRENT_LANGUAGE.store(language.index(), Ordering::Relaxed);
}

pub fn init_from_env() {
    if let Ok(tag) = std::env::var(LANGUAGE_ENV)
        && let Some(language) = Language::from_tag(&tag)
    {
        set_language(language);
    }
}

pub fn language() -> Language {
    Language::from_index(CURRENT_LANGUAGE.load(Ordering::Relaxed))
}

pub fn translate(id: &str, args: Option<&FluentArgs<'_>>) -> String {
    localizer().translate(language(), id, args)
}

struct Localizer {
    bundles: [FluentBundle<FluentResource>; 3],
}

impl Localizer {
    fn new() -> Self {
        Self {
            bundles: [
                bundle(langid!("en-US"), EN_US),
                bundle(langid!("zh-CN"), ZH_CN),
                bundle(langid!("ru-RU"), RU_RU),
            ],
        }
    }

    fn translate(&self, requested: Language, id: &str, args: Option<&FluentArgs<'_>>) -> String {
        std::iter::once(requested)
            .chain(
                FALLBACK_ORDER
                    .into_iter()
                    .filter(move |language| *language != requested),
            )
            .find_map(|language| self.format(language, id, args))
            .unwrap_or_else(|| id.to_owned())
    }

    fn format(
        &self,
        language: Language,
        id: &str,
        args: Option<&FluentArgs<'_>>,
    ) -> Option<String> {
        let message = self.bundles[language.index() as usize].get_message(id)?;
        let pattern = message.value()?;
        let mut errors = Vec::new();
        Some(
            self.bundles[language.index() as usize]
                .format_pattern(pattern, args, &mut errors)
                .into_owned(),
        )
    }
}

fn localizer() -> &'static Localizer {
    LOCALIZER.get_or_init(Localizer::new)
}

fn bundle(locale: LanguageIdentifier, source: &str) -> FluentBundle<FluentResource> {
    let resource = FluentResource::try_new(source.to_owned())
        .unwrap_or_else(|(_, errors)| panic!("invalid Fluent resource for {locale}: {errors:?}"));
    let mut bundle = FluentBundle::new_concurrent(vec![locale.clone()]);
    bundle
        .add_resource(resource)
        .unwrap_or_else(|errors| panic!("duplicate Fluent message for {locale}: {errors:?}"));
    bundle
}

#[macro_export]
macro_rules! tr {
    ($id:literal) => {
        $crate::i18n::translate($id, None)
    };
    ($id:literal, $($name:literal => $value:expr),+ $(,)?) => {{
        let mut args = fluent_bundle::FluentArgs::new();
        $(args.set($name, $value);)+
        $crate::i18n::translate($id, Some(&args))
    }};
}
