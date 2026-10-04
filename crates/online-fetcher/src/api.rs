use anyhow::{Context, Result, ensure};
use reqwest::blocking::{Client, Response};
use reqwest::{StatusCode, Url};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::fmt;
use std::time::Duration;

pub const DEFAULT_BASE_URL: &str = "https://firmwarefetcher.ljlvink.top";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FirmwareAction {
    Upgrade,
    Recovery,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RequestsQuery {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<FirmwareAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirmwarePackage {
    #[serde(rename = "versionNumber")]
    pub version_number: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirmwareRequest {
    pub id: u64,
    pub created_at: String,
    pub dev: String,
    pub vendor: String,
    pub upgrade_or_erec: FirmwareAction,
    #[serde(rename = "targetBlVersionNumber")]
    pub target_bl_version_number: String,
    #[serde(rename = "deviceName")]
    pub device_name: Option<String>,
    pub packages: Vec<FirmwarePackage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirmwareRequests {
    pub total: u64,
    pub page: u64,
    #[serde(rename = "perPage")]
    pub per_page: u64,
    pub requests: Vec<FirmwareRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewFirmwareRequest {
    pub dev: String,
    pub base: String,
    pub cust: String,
    pub preload: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    pub upgrade_or_erec: FirmwareAction,
}

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FirmwareFetcher HTTP {}: {}", self.status, self.message)
    }
}

impl std::error::Error for ApiError {}

#[derive(Debug, Clone)]
pub struct FirmwareClient {
    client: Client,
    requests_url: Url,
}

impl FirmwareClient {
    pub fn new() -> Result<Self> {
        Self::with_base_url(DEFAULT_BASE_URL)
    }

    pub fn with_base_url(base_url: &str) -> Result<Self> {
        let mut base = Url::parse(base_url).context("invalid FirmwareFetcher base URL")?;
        ensure!(
            matches!(base.scheme(), "http" | "https"),
            "FirmwareFetcher URL must use http:// or https://"
        );
        base.set_query(None);
        base.set_fragment(None);
        base.path_segments_mut()
            .map_err(|()| anyhow::anyhow!("FirmwareFetcher URL cannot be a base URL"))?
            .pop_if_empty()
            .push("requests");
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("haucet-online-fetcher/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("creating FirmwareFetcher HTTP client")?;
        Ok(Self {
            client,
            requests_url: base,
        })
    }

    pub fn list_requests(&self, query: &RequestsQuery) -> Result<FirmwareRequests> {
        let response = self
            .client
            .get(self.requests_url.clone())
            .query(query)
            .send()
            .context("querying firmware requests")?;
        decode_response(response, StatusCode::OK)
    }

    pub fn submit_request(&self, request: &NewFirmwareRequest) -> Result<FirmwareRequest> {
        let response = self
            .client
            .post(self.requests_url.clone())
            .json(request)
            .send()
            .context("submitting firmware request")?;
        decode_response(response, StatusCode::CREATED)
    }
}

fn decode_response<T: DeserializeOwned>(response: Response, expected: StatusCode) -> Result<T> {
    let status = response.status();
    if status != expected {
        #[derive(Deserialize)]
        struct ErrorBody {
            error: String,
        }
        let message = match response.json::<ErrorBody>() {
            Ok(body) => body.error,
            Err(error) => format!("invalid API error response: {error}"),
        };
        return Err(ApiError { status, message }.into());
    }
    response.json().context("decoding FirmwareFetcher response")
}
