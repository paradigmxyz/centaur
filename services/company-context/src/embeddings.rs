use anyhow::{Context, Result};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};

use crate::{config::Config, errors::rejected};

const EMBEDDING_BATCH_SIZE: usize = 25;

#[derive(Clone)]
pub struct EmbeddingsClient {
    http: Client,
    endpoint: String,
    api_key: String,
    model: String,
    dimensions: usize,
}

#[derive(Serialize)]
struct EmbeddingRequest<'a> {
    model: &'a str,
    input: &'a [String],
    dimensions: usize,
    encoding_format: &'static str,
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingItem>,
}

#[derive(Deserialize)]
struct EmbeddingItem {
    index: usize,
    embedding: Vec<f32>,
}

impl EmbeddingsClient {
    pub fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            http: Client::builder()
                .timeout(config.extraction_timeout)
                .build()?,
            endpoint: format!("{}/embeddings", config.openai_base_url),
            api_key: config.openai_api_key.clone(),
            model: config.embeddings_model.clone(),
            dimensions: config.embeddings_dimensions,
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn dimensions(&self) -> usize {
        self.dimensions
    }

    pub async fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>> {
        let mut output = Vec::with_capacity(inputs.len());
        for batch in inputs.chunks(EMBEDDING_BATCH_SIZE) {
            let request = EmbeddingRequest {
                model: &self.model,
                input: batch,
                dimensions: self.dimensions,
                encoding_format: "float",
            };
            let response = self
                .http
                .post(&self.endpoint)
                .bearer_auth(&self.api_key)
                .json(&request)
                .send()
                .await
                .context("send embeddings request")?;
            let status = response.status();
            if status.is_client_error()
                && !matches!(
                    status,
                    StatusCode::REQUEST_TIMEOUT
                        | StatusCode::TOO_MANY_REQUESTS
                        | StatusCode::UNAUTHORIZED
                        | StatusCode::FORBIDDEN
                )
            {
                return Err(rejected(format!(
                    "embeddings request was rejected with status {status}"
                )));
            }
            let response = response
                .error_for_status()
                .context("embeddings request failed")?
                .json::<EmbeddingResponse>()
                .await
                .map_err(|error| rejected(format!("decode embeddings response: {error}")))?;
            if response.data.len() != batch.len() {
                return Err(rejected(
                    "embedding response item count does not match request",
                ));
            }
            let mut ordered: Vec<Option<Vec<f32>>> = vec![None; batch.len()];
            for item in response.data {
                if item.index >= ordered.len() || ordered[item.index].is_some() {
                    return Err(rejected("embedding response contains an invalid index"));
                }
                if item.embedding.len() != self.dimensions {
                    return Err(rejected(format!(
                        "embedding response has dimension {}, expected {}",
                        item.embedding.len(),
                        self.dimensions
                    )));
                }
                ordered[item.index] = Some(item.embedding);
            }
            for embedding in ordered {
                output.push(
                    embedding.ok_or_else(|| rejected("embedding response is missing an index"))?,
                );
            }
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batches_have_a_bounded_size() {
        let inputs = vec![String::new(); 51];
        assert_eq!(
            inputs
                .chunks(EMBEDDING_BATCH_SIZE)
                .map(<[String]>::len)
                .collect::<Vec<_>>(),
            vec![25, 25, 1]
        );
    }
}
