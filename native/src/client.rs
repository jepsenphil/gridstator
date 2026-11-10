use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub version: u32,
    pub url: String,
    pub ca_pem: String,
    pub ca_fingerprint: String,
    pub token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_name: Option<String>,
}
#[derive(Clone)]
pub struct Client {
    pub config: Config,
    pub http: reqwest::Client,
}
#[derive(Debug)]
pub struct ControllerError {
    pub status: u16,
    pub code: Option<String>,
    pub message: String,
    pub hint: Option<String>,
    pub details: Option<Value>,
    pub cause: Option<Box<dyn std::error::Error + Send + Sync>>,
}
impl std::fmt::Display for ControllerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: ", self.status)?;
        if let Some(code) = &self.code {
            write!(formatter, "{code}: ")?;
        }
        formatter.write_str(&self.message)
    }
}
impl std::error::Error for ControllerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause
            .as_deref()
            .map(|cause| cause as &(dyn std::error::Error + 'static))
    }
}
pub fn normalize_url(url: &str) -> Result<String> {
    // Match JavaScript String.trim rather than Rust's Unicode White_Space set.
    let text = url.trim_matches(|character: char| {
        character == '\u{feff}' || (character.is_whitespace() && character != '\u{85}')
    });
    if text
        .get(..7)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("http://"))
    {
        return Err(ControllerError {
            status: 400,
            code: Some("BAD_REQUEST".into()),
            message: "Gridstator controllers only accept HTTPS connections".into(),
            hint: Some(format!("Use https://{}", &text[7..])),
            details: None,
            cause: None,
        }
        .into());
    }
    let candidate = if text
        .get(..8)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"))
    {
        text.to_owned()
    } else {
        format!("https://{text}")
    };
    let parsed = url::Url::parse(&candidate).map_err(|_| ControllerError {
        status: 400,
        code: Some("BAD_REQUEST".into()),
        message: format!("Invalid controller URL \"{url}\""),
        hint: None,
        details: None,
        cause: None,
    })?;
    // WHATWG URL normalizes an explicit HTTPS port 443 to absent, just as Node
    // does. The product default applies in either case. Never forward URL
    // userinfo, path, query or fragment to the pinned controller transport.
    let host = parsed.host_str().ok_or_else(|| ControllerError {
        status: 400,
        code: Some("BAD_REQUEST".into()),
        message: format!("Invalid controller URL \"{url}\""),
        hint: None,
        details: None,
        cause: None,
    })?;
    Ok(format!("https://{host}:{}", parsed.port().unwrap_or(7300)))
}
impl Client {
    fn network_error(&self, error: reqwest::Error) -> anyhow::Error {
        let mut source = Some(&error as &dyn std::error::Error);
        let mut certificate_error = None;
        while let Some(current) = source {
            if let Some(rustls::Error::InvalidCertificate(certificate)) =
                current.downcast_ref::<rustls::Error>()
            {
                certificate_error = Some(certificate.to_string());
                break;
            }
            // Hyper can wrap rustls in more than one io::Error. io::Error's
            // source skips its contained error, so inspect get_ref explicitly.
            source = current
                .downcast_ref::<std::io::Error>()
                .and_then(std::io::Error::get_ref)
                .map(|inner| inner as &dyn std::error::Error)
                .or_else(|| current.source());
        }
        if let Some(certificate) = certificate_error {
            ControllerError {
                status: 495,
                code: Some("TLS_PIN_MISMATCH".into()),
                message: format!("The controller at {} presented a certificate that is not signed by the pinned cluster CA ({certificate})", self.config.url),
                hint: Some("If the controller was re-initialized, log in again with the new fingerprint. Otherwise something may be intercepting the connection.".into()),
                details: None,
                cause: Some(Box::new(error)),
            }.into()
        } else {
            error.into()
        }
    }
    pub fn new(config: Config) -> Result<Self> {
        Ok(Self {
            http: crate::pki::http_client(&config.ca_pem)?,
            config,
        })
    }
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let path = path
            .map(std::path::PathBuf::from)
            .unwrap_or_else(crate::state::config_path);
        let saved = std::fs::read(&path)
            .ok()
            .and_then(|s| serde_json::from_slice::<Config>(&s).ok());
        let url = std::env::var("GRIDSTATOR_URL")
            .ok()
            .or_else(|| saved.as_ref().map(|s| s.url.clone()))
            .context("not connected; run gridstator login")?;
        let token = std::env::var("GRIDSTATOR_TOKEN")
            .ok()
            .or_else(|| saved.as_ref().map(|s| s.token.clone()))
            .context("missing controller token")?;
        let ca = match std::env::var("GRIDSTATOR_CA_FILE") {
            Ok(p) => std::fs::read_to_string(p)?,
            Err(_) => saved
                .as_ref()
                .map(|s| s.ca_pem.clone())
                .context("missing pinned cluster CA")?,
        };
        Self::new(Config {
            version: 1,
            url: normalize_url(&url)?,
            ca_fingerprint: crate::pki::fingerprint(&ca)?,
            ca_pem: ca,
            token,
            token_name: saved.and_then(|s| s.token_name),
        })
    }
    pub async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let mut request = self
            .http
            .request(method.clone(), format!("{}{}", self.config.url, path))
            .bearer_auth(&self.config.token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let mut response = request
            .send()
            .await
            .map_err(|error| self.network_error(error))?;
        let status = response.status();
        let limit = if status.is_success() {
            16 * 1024 * 1024
        } else {
            1024 * 1024
        };
        let mut bytes = Vec::new();
        let mut oversized = false;
        loop {
            let chunk = match response.chunk().await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(error) if status.is_success() => return Err(self.network_error(error)),
                Err(_) => {
                    oversized = true;
                    break;
                }
            };
            if bytes.len() + chunk.len() > limit {
                oversized = true;
                break;
            }
            bytes.extend_from_slice(&chunk);
        }
        if oversized && status.is_success() {
            return Err(ControllerError {
                status: 413,
                code: Some("PAYLOAD_TOO_LARGE".into()),
                message: "Response too large".into(),
                hint: None,
                details: None,
                cause: None,
            }
            .into());
        }
        let value: Option<Value> = if oversized {
            None
        } else {
            serde_json::from_slice(&bytes).ok()
        };
        if !status.is_success() {
            let envelope = value
                .as_ref()
                .and_then(|value| value.get("error"))
                .filter(|value| value.is_object() || value.is_array());
            let fallback = match status.as_u16() {
                400 => "BAD_REQUEST",
                401 => "UNAUTHORIZED",
                403 => "FORBIDDEN",
                404 => "NOT_FOUND",
                409 => "CONFLICT",
                413 => "PAYLOAD_TOO_LARGE",
                503 => "UNAVAILABLE",
                _ => "INTERNAL",
            };
            let code = envelope
                .and_then(|error| error.get("code"))
                .and_then(Value::as_str)
                .filter(|code| {
                    matches!(
                        *code,
                        "BAD_REQUEST"
                            | "UNAUTHORIZED"
                            | "FORBIDDEN"
                            | "NOT_FOUND"
                            | "CONFLICT"
                            | "PAYLOAD_TOO_LARGE"
                            | "UNAVAILABLE"
                            | "TLS_PIN_MISMATCH"
                            | "CONFIG"
                            | "INTERNAL"
                    )
                })
                .unwrap_or(fallback);
            let message = envelope
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    if envelope.is_some() {
                        format!("HTTP {}", status.as_u16())
                    } else {
                        format!("Controller responded with HTTP {}", status.as_u16())
                    }
                });
            return Err(ControllerError {
                status: status.as_u16(),
                code: Some(code.into()),
                message,
                hint: envelope
                    .and_then(|error| error.get("hint"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                details: envelope.and_then(|error| error.get("details")).cloned(),
                cause: None,
            }
            .into());
        }
        if bytes.is_empty() || status.as_u16() == 204 {
            return Ok(Value::Null);
        }
        value.with_context(|| format!("Controller returned invalid JSON for {method} {path}"))
    }
    pub async fn get(&self, path: &str) -> Result<Value> {
        self.request(reqwest::Method::GET, path, None).await
    }
    pub async fn post(&self, path: &str, value: Value) -> Result<Value> {
        self.request(reqwest::Method::POST, path, Some(value)).await
    }
    /// Keep indefinite draining explicit. Omitting it on older controllers can
    /// silently enable cancellation after one hour, so incompatibility is never
    /// retried with a different request.
    pub async fn start_update(&self, mut input: Value) -> Result<Value> {
        let object = input
            .as_object_mut()
            .context("BAD_REQUEST: update request must be an object")?;
        if object.get("drainTimeoutSeconds").is_none_or(Value::is_null) {
            if object.get("now").and_then(Value::as_bool) == Some(true) {
                object.remove("drainTimeoutSeconds");
            } else {
                object.insert("drainTimeoutSeconds".into(), Value::Null);
            }
        }
        let indefinite =
            input["drainTimeoutSeconds"].is_null() && input.get("drainTimeoutSeconds").is_some();
        match self.post("/v1/updates", input).await {
            Err(error)
                if indefinite
                    && error.downcast_ref::<ControllerError>().is_some_and(|error| {
                        error.code.as_deref() == Some("BAD_REQUEST")
                            && error.message == "Invalid update request: drainTimeoutSeconds: Invalid input: expected number, received null"
                    }) =>
            {
                let cause = error.downcast::<ControllerError>().unwrap();
                Err(ControllerError {
                    status: 409,
                    code: Some("CONFLICT".into()),
                    message: "This controller cannot wait indefinitely for running jobs during worker updates. Update the controller first, then retry the worker update.".into(),
                    hint: Some("Install a current controller native release bundle with `gridstator controller publish-build <bundle.tar.gz>`.".into()),
                    details: None,
                    cause: Some(Box::new(cause)),
                }.into())
            }
            result => result,
        }
    }
    pub async fn login(url: &str, pin: &str, token: &str, path: &Path) -> Result<Self> {
        let url = normalize_url(url)?;
        let ca = crate::pki::fetch_pinned_ca(&url, pin).await?;
        let mut client = Self::new(Config {
            version: 1,
            url,
            ca_pem: ca,
            ca_fingerprint: crate::pki::normalize_fingerprint(pin)?,
            token: token.into(),
            token_name: None,
        })?;
        let cluster = client.get("/v1/cluster").await?;
        ensure!(
            cluster["version"]
                .as_str()
                .and_then(|version| semver::Version::parse(version).ok())
                .is_some_and(|version| version.major == 2),
            "CONFIG: this native CLI requires a v2 controller; create a fresh native cluster"
        );
        client.config.token_name = cluster
            .pointer("/whoami/name")
            .and_then(Value::as_str)
            .map(str::to_owned);
        crate::state::write_json(path, &serde_json::to_value(&client.config)?)?;
        Ok(client)
    }
}
