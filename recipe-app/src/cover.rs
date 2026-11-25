//! Server-side OpenRouter image generation. The HTTP client is injected so callers
//! and tests can keep provider traffic behind a local fake.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;

pub const DEFAULT_MODEL: &str = "black-forest-labs/flux-3-image-klein";
pub const DEFAULT_PROMPT: &str = "A natural, unretouched photograph of a home-cooked dish, as it might appear in a good home-cooking magazine. Soft daylight from a window on the left, overcast and diffuse, with gentle shadows. True-to-life colours, slightly muted, with no colour grading. Realistic textures: some uneven browning, a few crumbs or drips, sauce that isn't perfectly placed. Food that looks cooked by a skilled home cook, not styled for an advert.\n\nSetting: the dish sits on a red-and-white gingham tablecloth with washed-out, slightly faded cotton and soft creases, on a rustic wooden table. Plain off-white or cream ceramic plate or bowl, simple cutlery. Little else in frame: at most one small, relevant ingredient or a linen napkin, slightly out of focus.\n\nCamera: 45-degree angle, 50mm lens, shallow depth of field, focus on the front of the dish. The whole dish sits within the centre of the frame, with space around it for cropping. No text, labels or watermarks.";
pub const MAX_DISH_DESCRIPTION_CHARS: usize = 500;
pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

#[derive(Clone)]
pub struct Generator {
    client: reqwest::Client,
    key: String,
    model: String,
    house_prompt: String,
    endpoint: String,
}

#[derive(Deserialize)]
struct ImagesResponse {
    data: Vec<ImageData>,
}
#[derive(Deserialize)]
struct ImageData {
    b64_json: String,
}

impl Generator {
    pub fn new(
        key: String,
        model: Option<String>,
        prompt: Option<String>,
    ) -> Result<Self, &'static str> {
        if key.trim().is_empty() {
            return Err("cover generation is not configured");
        }
        let house_prompt = prompt.unwrap_or_else(|| DEFAULT_PROMPT.to_owned());
        if house_prompt.trim().is_empty() || house_prompt.len() > 32_000 {
            return Err("cover prompt configuration is invalid");
        }
        Self::build(
            key,
            model,
            house_prompt,
            "https://openrouter.ai/api/v1/images".to_owned(),
        )
    }

    /// Test-only endpoint override. Only loopback HTTP URLs are accepted so this
    /// cannot be used to route production credentials to an arbitrary host.
    pub fn with_test_endpoint(
        key: String,
        model: Option<String>,
        prompt: Option<String>,
        endpoint: &str,
    ) -> Result<Self, &'static str> {
        let parsed = reqwest::Url::parse(endpoint).map_err(|_| "invalid test endpoint")?;
        let loopback = parsed.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        });
        if parsed.scheme() != "http" || !loopback {
            return Err("test endpoint must use loopback HTTP");
        }
        let house_prompt = prompt.unwrap_or_else(|| DEFAULT_PROMPT.to_owned());
        Self::build(key, model, house_prompt, endpoint.to_owned())
    }

    fn build(
        key: String,
        model: Option<String>,
        house_prompt: String,
        endpoint: String,
    ) -> Result<Self, &'static str> {
        if key.trim().is_empty() {
            return Err("cover generation is not configured");
        }
        if house_prompt.trim().is_empty() || house_prompt.len() > 32_000 {
            return Err("cover prompt configuration is invalid");
        }
        Ok(Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .timeout(std::time::Duration::from_secs(90))
                .build()
                .map_err(|_| "cover generation is not configured")?,
            key,
            model: model
                .filter(|m| !m.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_MODEL.to_owned()),
            house_prompt,
            endpoint,
        })
    }

    /// Performs exactly one request. Provider response bodies are never surfaced.
    pub async fn generate(&self, dish_description: &str) -> Result<Vec<u8>, &'static str> {
        let dish = dish_description.trim();
        if dish.is_empty() || dish.chars().count() > MAX_DISH_DESCRIPTION_CHARS {
            return Err("dish description must contain 1–500 characters");
        }
        let prompt = format!("{}\n\nDish: {}", self.house_prompt, dish);
        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.key)
            .json(&serde_json::json!({"model":self.model,"prompt":prompt}))
            .send()
            .await
            .map_err(|_| "cover provider request failed")?;
        if !response.status().is_success() {
            return Err("cover provider request failed");
        }
        if response
            .content_length()
            .is_some_and(|length| length > (MAX_IMAGE_BYTES * 2) as u64)
        {
            return Err("cover provider output exceeds size limit");
        }
        let mut response = response;
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "cover provider returned invalid output")?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_IMAGE_BYTES * 2 {
                return Err("cover provider output exceeds size limit");
            }
            bytes.extend_from_slice(&chunk);
        }
        if bytes.len() > MAX_IMAGE_BYTES * 2 {
            return Err("cover provider output exceeds size limit");
        }
        let parsed: ImagesResponse =
            serde_json::from_slice(&bytes).map_err(|_| "cover provider returned invalid output")?;
        let encoded = parsed
            .data
            .first()
            .ok_or("cover provider returned no image")?
            .b64_json
            .as_str();
        if encoded.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 {
            return Err("cover provider output exceeds size limit");
        }
        let image = STANDARD
            .decode(encoded)
            .map_err(|_| "cover provider returned invalid image data")?;
        if image.is_empty() || image.len() > MAX_IMAGE_BYTES {
            return Err("cover provider output exceeds size limit");
        }
        Ok(image)
    }
}
