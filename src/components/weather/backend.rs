use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;

pub struct WeatherBackend;

impl WeatherBackend {
    pub fn fetch_json(&self, url: &str, timeout_seconds: u8) -> Result<Vec<u8>> {
        self.fetch(url, timeout_seconds, None)
    }

    pub fn reverse_location(&self, latitude: f64, longitude: f64) -> Result<Option<String>> {
        let url = format!(
            "https://nominatim.openstreetmap.org/reverse?format=jsonv2&lat={latitude}&lon={longitude}"
        );
        let bytes = self.fetch(&url, 8, Some("Raylander/0.1"))?;
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).context("Nominatim returned invalid JSON")?;
        let address = &value["address"];
        Ok(["city", "town", "village", "municipality", "county"]
            .iter()
            .find_map(|key| address[*key].as_str())
            .map(|name| name.strip_suffix(" County").unwrap_or(name).to_owned()))
    }

    fn fetch(&self, url: &str, timeout_seconds: u8, user_agent: Option<&str>) -> Result<Vec<u8>> {
        let client = Client::builder()
            .timeout(Duration::from_secs(u64::from(timeout_seconds)))
            .build()
            .context("failed to create weather HTTP client")?;
        let mut request = client.get(url);
        if let Some(user_agent) = user_agent {
            request = request.header(reqwest::header::USER_AGENT, user_agent);
        }
        let response = request
            .send()
            .with_context(|| format!("failed to execute weather request for {url}"))?;
        if !response.status().is_success() {
            bail!(
                "weather request failed with HTTP status {} for {url}",
                response.status()
            );
        }
        Ok(response
            .bytes()
            .with_context(|| format!("failed to read weather response for {url}"))?
            .to_vec())
    }
}
