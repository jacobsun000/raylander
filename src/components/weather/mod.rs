use std::{
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use axum::{
    Router,
    extract::Query,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;

use crate::components::{Component, ComponentId, ComponentView, GridSize};

use self::backend::WeatherBackend;

mod backend;

pub struct Weather;

impl Component for Weather {
    fn id(&self) -> ComponentId {
        ComponentId::new("weather")
    }

    fn size(&self) -> GridSize {
        GridSize::ONE_BY_ONE
    }

    fn render(&self) -> anyhow::Result<ComponentView> {
        Ok(ComponentView {
            body: include_str!("view.html").to_owned(),
            overlays: include_str!("overlays.html").to_owned(),
        })
    }

    fn routes(&self) -> Router {
        routes()
    }
}

static CONFIGURED_COORDINATES: OnceLock<(String, String)> = OnceLock::new();
static FRAGMENT: OnceLock<Mutex<Option<(Instant, String)>>> = OnceLock::new();

pub(crate) struct WeatherConfig {
    pub latitude: String,
    pub longitude: String,
}

pub(crate) fn configure(config: WeatherConfig) {
    let _ = CONFIGURED_COORDINATES.set((config.latitude, config.longitude));
}

pub(crate) fn configured_coordinates() -> Option<&'static (String, String)> {
    CONFIGURED_COORDINATES.get()
}

pub(crate) fn routes() -> Router {
    Router::new()
        .route("/details", get(weather_details))
        .route("/location-name", get(location_name))
        .route(
            "/component.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("component.js"),
                )
            }),
        )
}

#[derive(Deserialize)]
struct WeatherResponse {
    current: CurrentWeather,
    daily: DailyWeather,
}

#[derive(Deserialize)]
struct CurrentWeather {
    temperature_2m: f32,
    weather_code: u8,
}

#[derive(Deserialize)]
struct DailyWeather {
    temperature_2m_min: Vec<f32>,
    temperature_2m_max: Vec<f32>,
}

pub(crate) fn update_fragment() -> String {
    let mut cache = FRAGMENT
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if cache
        .as_ref()
        .is_none_or(|(at, _)| at.elapsed() >= Duration::from_secs(15 * 60))
        && let Some(fragment) = query_fragment()
    {
        *cache = Some((Instant::now(), fragment));
    }
    cache
        .as_ref()
        .map(|(_, fragment)| fragment.clone())
        .unwrap_or_default()
}

fn query_fragment() -> Option<String> {
    let (latitude, longitude) = configured_coordinates()?;
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={latitude}&longitude={longitude}&current=temperature_2m,weather_code&daily=temperature_2m_min,temperature_2m_max&forecast_days=1&timezone=auto"
    );
    let bytes = WeatherBackend.fetch_json(&url, 5).ok()?;
    let weather: WeatherResponse = serde_json::from_slice(&bytes).ok()?;
    let (icon, label) = weather_description(weather.current.weather_code);
    let low = weather.daily.temperature_2m_min.first()?;
    let high = weather.daily.temperature_2m_max.first()?;
    let mut html = String::new();
    for (id, value) in [
        ("weather-icon", icon.to_owned()),
        (
            "weather-temp",
            format!("{:.0}°C", weather.current.temperature_2m),
        ),
        ("weather-low", format!("{low:.0}°")),
        ("weather-high", format!("{high:.0}°")),
        ("weather-label", label.to_owned()),
    ] {
        html.push_str(&format!(
            r#"<span id="{id}" hx-swap-oob="innerHTML">{value}</span>"#
        ));
    }
    Some(html)
}

fn weather_description(code: u8) -> (&'static str, &'static str) {
    match code {
        0 => ("☀", "Clear"),
        1..=3 => ("◑", "Cloudy"),
        45 | 48 => ("≋", "Fog"),
        51..=67 | 80..=82 => ("☂", "Rain"),
        71..=77 | 85 | 86 => ("❄", "Snow"),
        95..=99 => ("ϟ", "Storm"),
        _ => ("·", "Weather"),
    }
}

#[derive(Deserialize)]
struct WeatherDetailsQuery {
    city: Option<String>,
    latitude: Option<f64>,
    longitude: Option<f64>,
}

#[derive(Deserialize)]
struct LocationQuery {
    latitude: f64,
    longitude: f64,
}

async fn location_name(Query(query): Query<LocationQuery>) -> Response {
    if !valid_coordinates(query.latitude, query.longitude) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match tokio::task::spawn_blocking(move || {
        WeatherBackend.reverse_location(query.latitude, query.longitude)
    })
    .await
    {
        Ok(Ok(Some(name))) => name.into_response(),
        Ok(Ok(None)) => StatusCode::NOT_FOUND.into_response(),
        _ => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn weather_details(Query(query): Query<WeatherDetailsQuery>) -> Response {
    let phone = query
        .latitude
        .zip(query.longitude)
        .filter(|(latitude, longitude)| valid_coordinates(*latitude, *longitude))
        .map(|(latitude, longitude)| (latitude.to_string(), longitude.to_string()));
    let configured = CONFIGURED_COORDINATES.get();
    let (latitude, longitude) = match query.city.as_deref().unwrap_or("minneapolis") {
        "minneapolis" => phone
            .as_ref()
            .map(|(lat, lon)| (lat.as_str(), lon.as_str()))
            .or_else(|| configured.map(|(lat, lon)| (lat.as_str(), lon.as_str())))
            .unwrap_or(("44.9778", "-93.2650")),
        "los-angeles" => ("34.0522", "-118.2437"),
        "detroit" => ("42.3314", "-83.0458"),
        "new-york" => ("40.7128", "-74.0060"),
        "guangzhou" => ("23.1291", "113.2644"),
        "configured" => match configured {
            Some((lat, lon)) => (lat.as_str(), lon.as_str()),
            None => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        },
        _ => return StatusCode::BAD_REQUEST.into_response(),
    };
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={latitude}&longitude={longitude}&current=temperature_2m,apparent_temperature,relative_humidity_2m,weather_code,wind_speed_10m&hourly=temperature_2m,precipitation_probability,weather_code&daily=weather_code,temperature_2m_min,temperature_2m_max,precipitation_probability_max,sunrise,sunset&forecast_days=7&timezone=auto"
    );
    match tokio::task::spawn_blocking(move || WeatherBackend.fetch_json(&url, 8)).await {
        Ok(Ok(bytes)) => (
            [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
            bytes,
        )
            .into_response(),
        _ => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

fn valid_coordinates(latitude: f64, longitude: f64) -> bool {
    (-90.0..=90.0).contains(&latitude) && (-180.0..=180.0).contains(&longitude)
}
