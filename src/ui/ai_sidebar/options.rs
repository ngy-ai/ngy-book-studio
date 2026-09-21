//! Thread, endpoint and book options offered in the sidebar.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AiThreadOption {
    pub id: String,
    pub title: String,
    pub book_ids: Vec<String>,
}

impl AiThreadOption {
    pub(crate) fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        book_ids: Vec<String>,
    ) -> Self {
        let title = title.into();
        Self {
            id: id.into(),
            title: non_empty_label(title, "新对话"),
            book_ids,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AiEndpointStatus {
    Local,
    Remote { hostname: String },
    Unavailable,
}

impl AiEndpointStatus {
    pub(super) fn from_base_url(base_url: &str) -> Self {
        let Ok(url) = normalize_provider_base_url(base_url) else {
            return Self::Unavailable;
        };
        let Some(hostname) = url
            .host_str()
            .map(|host| host.trim_matches(['[', ']']).to_string())
        else {
            return Self::Unavailable;
        };
        let loopback = hostname.eq_ignore_ascii_case("localhost")
            || hostname
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback());
        if loopback {
            Self::Local
        } else {
            Self::Remote { hostname }
        }
    }

    pub(super) fn current(services: &AppServices) -> Self {
        services
            .provider_settings()
            .map(|settings| Self::from_settings(&settings))
            .unwrap_or(Self::Unavailable)
    }

    pub(super) fn from_settings(settings: &ProviderSettings) -> Self {
        let mut remote = Vec::new();
        for (role, label) in [
            (ModelRole::Chat, "问答"),
            (ModelRole::Translation, "翻译"),
            (ModelRole::Embedding, "Embedding"),
            (ModelRole::Vision, "视觉"),
        ] {
            let Ok(endpoint) = settings.endpoint_for(role) else {
                return Self::Unavailable;
            };
            match Self::from_base_url(&endpoint.base_url) {
                Self::Local => {}
                Self::Remote { hostname } => remote.push(format!("{label}：{hostname}")),
                Self::Unavailable => return Self::Unavailable,
            }
        }
        if remote.is_empty() {
            Self::Local
        } else {
            Self::Remote {
                hostname: remote.join("；"),
            }
        }
    }

    pub(super) fn label(&self) -> String {
        match self {
            Self::Local => "本地".to_string(),
            Self::Remote { hostname } => format!("远程 · {hostname}"),
            Self::Unavailable => "端点不可用".to_string(),
        }
    }

    pub(super) fn is_warning(&self) -> bool {
        !matches!(self, Self::Local)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AiBookOption {
    pub id: String,
    pub title: String,
}

impl AiBookOption {
    pub(crate) fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        let id = id.into();
        let title = title.into();
        Self {
            id,
            title: if title.trim().is_empty() {
                "未命名图书".to_string()
            } else {
                title
            },
        }
    }
}
