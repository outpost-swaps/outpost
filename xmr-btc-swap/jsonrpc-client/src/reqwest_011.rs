use crate::{Response, SendRequest, Url};
use reqwest_011::header::CONTENT_TYPE;
use serde::de::DeserializeOwned;

#[async_trait::async_trait]
impl SendRequest for reqwest_011::Client {
    type Error = reqwest_011::Error;

    async fn send_request<P>(&self, endpoint: Url, body: String) -> Result<Response<P>, Self::Error>
    where
        P: DeserializeOwned,
    {
        Ok(self
            .post(endpoint)
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await?
            .json()
            .await?)
    }
}

impl From<reqwest_011::Error> for crate::Error<reqwest_011::Error> {
    fn from(inner: reqwest_011::Error) -> Self {
        crate::Error::Client(inner)
    }
}
