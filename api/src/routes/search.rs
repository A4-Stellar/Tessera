use axum::{extract::Query, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct SearchQuery {
    pub q: String,
}

#[derive(Serialize)]
pub struct SearchResult {
    pub id: String,
    pub title: String,
}

pub async fn search_handler(Query(query): Query<SearchQuery>) -> impl IntoResponse {
    let result = vec![SearchResult {
        id: "123".to_string(),
        title: format!("Match for {}", query.q),
    }];
    Json(result)
}
