//! S3-compatible object storage client.
//!
//! The same code talks to MinIO locally and Tigris or AWS S3 on the VPS;
//! only `S3_*` environment variables change. Credentials are static keys
//! from the environment.

use std::time::Duration;

use aws_sdk_s3::{
    config::{BehaviorVersion, Builder, Credentials, Region},
    error::DisplayErrorContext,
    presigning::PresigningConfig,
    primitives::ByteStream,
    types::{BucketLocationConstraint, CreateBucketConfiguration},
    Client,
};

use crate::{config::S3Config, error::AppError};

/// Handle to the configured bucket. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Storage {
    client: Client,
    bucket: String,
    region: String,
}

impl Storage {
    /// Builds a client from config. Does not touch the network.
    pub fn new(cfg: &S3Config) -> Self {
        let credentials = Credentials::new(
            cfg.access_key.expose(),
            cfg.secret_key.expose(),
            None,
            None,
            "voice-env",
        );
        let mut builder = Builder::new()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(cfg.region.clone()))
            .credentials_provider(credentials)
            .force_path_style(cfg.force_path_style);
        if let Some(endpoint) = &cfg.endpoint {
            builder = builder.endpoint_url(endpoint);
        }
        Self {
            client: Client::from_conf(builder.build()),
            bucket: cfg.bucket.clone(),
            region: cfg.region.clone(),
        }
    }

    /// The underlying SDK client.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// The configured bucket name.
    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    /// Readiness probe: checks that the bucket exists and is reachable.
    pub async fn ping(&self) -> Result<(), AppError> {
        self.client
            .head_bucket()
            .bucket(&self.bucket)
            .send()
            .await
            .map(|_| ())
            .map_err(|e| AppError::Storage(DisplayErrorContext(&e).to_string()))
    }

    /// Creates the bucket if it does not exist. Used for local MinIO only;
    /// production buckets are provisioned outside the app.
    pub async fn ensure_bucket(&self) -> Result<(), AppError> {
        if self.ping().await.is_ok() {
            return Ok(());
        }
        let mut request = self.client.create_bucket().bucket(&self.bucket);
        if self.region != "us-east-1" {
            request = request.create_bucket_configuration(
                CreateBucketConfiguration::builder()
                    .location_constraint(BucketLocationConstraint::from(self.region.as_str()))
                    .build(),
            );
        }
        request
            .send()
            .await
            .map(|_| ())
            .map_err(|e| AppError::Storage(DisplayErrorContext(&e).to_string()))
    }

    /// Uploads an object.
    pub async fn put(&self, key: &str, body: Vec<u8>, content_type: &str) -> Result<(), AppError> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .content_type(content_type)
            .body(ByteStream::from(body))
            .send()
            .await
            .map(|_| ())
            .map_err(|e| AppError::Storage(DisplayErrorContext(&e).to_string()))
    }

    /// Downloads an object.
    pub async fn get(&self, key: &str) -> Result<Vec<u8>, AppError> {
        let out = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| AppError::Storage(DisplayErrorContext(&e).to_string()))?;
        let data = out
            .body
            .collect()
            .await
            .map_err(|e| AppError::Storage(e.to_string()))?;
        Ok(data.into_bytes().to_vec())
    }

    /// Deletes an object (a missing object is not an error).
    pub async fn delete(&self, key: &str) -> Result<(), AppError> {
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map(|_| ())
            .map_err(|e| AppError::Storage(DisplayErrorContext(&e).to_string()))
    }

    /// A short-lived signed GET URL.
    pub async fn presign_get(&self, key: &str, ttl: Duration) -> Result<String, AppError> {
        let config =
            PresigningConfig::expires_in(ttl).map_err(|e| AppError::Storage(e.to_string()))?;
        let req = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .presigned(config)
            .await
            .map_err(|e| AppError::Storage(DisplayErrorContext(&e).to_string()))?;
        Ok(req.uri().to_string())
    }
}
