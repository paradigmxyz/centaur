use anyhow::{Context, Result, bail};
use reqwest::{Client, RequestBuilder};
use serde::{Deserialize, Serialize};

use crate::config::Config;

const EMBEDDING_BATCH_SIZE: usize = 25;

#[derive(Clone)]
pub struct EmbeddingsClient {
    http: Client,
    endpoint: String,
    api_key: Option<String>,
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
            let builder = self.http.post(&self.endpoint).json(&request);
            let response = self
                .authorize(builder)
                .send()
                .await
                .context("send embeddings request")?
                .error_for_status()
                .context("embeddings request failed")?
                .json::<EmbeddingResponse>()
                .await
                .context("decode embeddings response")?;
            if response.data.len() != batch.len() {
                bail!("embedding response item count does not match request");
            }
            let mut ordered: Vec<Option<Vec<f32>>> = vec![None; batch.len()];
            for item in response.data {
                if item.index >= ordered.len() || ordered[item.index].is_some() {
                    bail!("embedding response contains an invalid index");
                }
                if item.embedding.len() != self.dimensions {
                    bail!(
                        "embedding response has dimension {}, expected {}",
                        item.embedding.len(),
                        self.dimensions
                    );
                }
                ordered[item.index] = Some(item.embedding);
            }
            for embedding in ordered {
                output.push(embedding.context("embedding response is missing an index")?);
            }
        }
        Ok(output)
    }

    fn authorize(&self, request: RequestBuilder) -> RequestBuilder {
        match &self.api_key {
            Some(api_key) => request.bearer_auth(api_key),
            None => request,
        }
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
