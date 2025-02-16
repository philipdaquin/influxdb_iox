//! Client helpers for writing end to end ng tests
use arrow::record_batch::RecordBatch;
use futures::{stream::FuturesUnordered, StreamExt};
use http::Response;
use hyper::{Body, Client, Request};
use influxdb_iox_client::{
    connection::Connection,
    flight::generated_types::read_info::QueryType,
    flight::generated_types::ReadInfo,
    write_info::generated_types::{merge_responses, GetWriteInfoResponse, ShardStatus},
};
use observability_deps::tracing::info;
use std::time::Duration;

/// Writes the line protocol to the write_base/api/v2/write endpoint (typically on the router)
pub async fn write_to_router(
    line_protocol: impl Into<String>,
    org: impl AsRef<str>,
    bucket: impl AsRef<str>,
    write_base: impl AsRef<str>,
) -> Response<Body> {
    let client = Client::new();
    let url = format!(
        "{}/api/v2/write?org={}&bucket={}",
        write_base.as_ref(),
        org.as_ref(),
        bucket.as_ref()
    );

    let request = Request::builder()
        .uri(url)
        .method("POST")
        .body(Body::from(line_protocol.into()))
        .expect("failed to construct HTTP request");

    client
        .request(request)
        .await
        .expect("http error sending write")
}

/// Extracts the write token from the specified response (to the /api/v2/write api)
pub fn get_write_token(response: &Response<Body>) -> String {
    let message = format!("no write token in {:?}", response);
    response
        .headers()
        .get("X-IOx-Write-Token")
        .expect(&message)
        .to_str()
        .expect("Value not a string")
        .to_string()
}

/// returns the write info from the connection (to either an ingester or a querier) for this token
pub async fn token_info(
    write_token: impl AsRef<str>,
    connection: Connection,
) -> Result<GetWriteInfoResponse, influxdb_iox_client::error::Error> {
    influxdb_iox_client::write_info::Client::new(connection)
        .get_write_info(write_token.as_ref())
        .await
}

/// returns a combined write info that contains the combined
/// information across all ingester_connections for all the specified
/// tokens
pub async fn combined_token_info(
    write_tokens: Vec<String>,
    ingester_connections: Vec<Connection>,
) -> Result<GetWriteInfoResponse, influxdb_iox_client::error::Error> {
    let responses = write_tokens
        .into_iter()
        .flat_map(|write_token| {
            ingester_connections
                .clone()
                .into_iter()
                .map(move |ingester_connection| {
                    token_info(write_token.clone(), ingester_connection)
                })
        })
        .collect::<FuturesUnordered<_>>()
        .collect::<Vec<_>>()
        .await
        // check for errors
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;

    info!("combining response: {:#?}", responses);

    // merge them together
    Ok(merge_responses(responses))
}

/// returns true if the write for this token is persisted, false if it
/// is not persisted. panic's on error
pub async fn token_is_persisted(
    write_token: impl AsRef<str>,
    ingester_connection: Connection,
) -> bool {
    let res = token_info(write_token, ingester_connection)
        .await
        .expect("Error fetching write info for token");
    all_persisted(&res)
}

const MAX_QUERY_RETRY_TIME_SEC: u64 = 20;

/// Waits for the specified predicate to return true
pub async fn wait_for_token<F>(write_token: impl Into<String>, connection: Connection, f: F)
where
    F: Fn(&GetWriteInfoResponse) -> bool,
{
    let write_token = write_token.into();
    assert!(!write_token.is_empty());

    info!("  write token: {}", write_token);

    let retry_duration = Duration::from_secs(MAX_QUERY_RETRY_TIME_SEC);
    let mut write_info_client = influxdb_iox_client::write_info::Client::new(connection);
    tokio::time::timeout(retry_duration, async move {
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        loop {
            match write_info_client.get_write_info(&write_token).await {
                Ok(res) => {
                    if f(&res) {
                        return;
                    }
                    info!("Retrying; predicate not satistified: {:?}", res);
                }

                Err(e) => {
                    info!("Retrying; Got error getting write_info: {}", e);
                }
            };
            interval.tick().await;
        }
    })
    .await
    .expect("did not get passing predicate on token");
}

/// Waits for the specified write token to be readable
pub async fn wait_for_readable(write_token: impl Into<String>, connection: Connection) {
    info!("Waiting for write token to be readable");

    wait_for_token(write_token, connection, |res| {
        if all_readable(res) {
            info!("Write is readable: {:?}", res);
            true
        } else {
            false
        }
    })
    .await
}

/// Waits for the write token to be persisted
pub async fn wait_for_persisted(write_token: impl Into<String>, connection: Connection) {
    info!("Waiting for write token to be persisted");

    wait_for_token(write_token, connection, |res| {
        if all_persisted(res) {
            info!("Write is persisted: {:?}", res);
            true
        } else {
            false
        }
    })
    .await
}

/// returns true if all shards in the response are readable
/// TODO: maybe put this in the influxdb_iox_client library / make a
/// proper public facing client API. For now, iterate in the end to end tests.
pub fn all_readable(res: &GetWriteInfoResponse) -> bool {
    res.shard_infos.iter().all(|info| {
        matches!(
            info.status(),
            ShardStatus::Readable | ShardStatus::Persisted
        )
    })
}

/// returns true if all shards in the response are persisted
/// TODO: maybe put this in the influxdb_iox_client library / make a
/// proper public facing client API. For now, iterate in the end to end tests.
pub fn all_persisted(res: &GetWriteInfoResponse) -> bool {
    res.shard_infos
        .iter()
        .all(|info| matches!(info.status(), ShardStatus::Persisted))
}

/// Runs a query using the flight API on the specified connection.
pub async fn try_run_query(
    sql_query: impl Into<String>,
    query_type: QueryType,
    namespace: impl Into<String>,
    querier_connection: Connection,
) -> Result<Vec<RecordBatch>, influxdb_iox_client::flight::Error> {
    let sql_query = sql_query.into();
    let namespace_name = namespace.into();

    let mut client = influxdb_iox_client::flight::Client::new(querier_connection);

    // This does nothing except test the client handshake implementation.
    client.handshake().await?;

    let mut response = client
        .perform_query(ReadInfo {
            namespace_name,
            sql_query,
            query_type: query_type.into(),
        })
        .await?;

    response.collect().await
}

/// Runs a SQL query using the flight API on the specified connection.
///
/// This is similar ot [`run_sql`] but does NOT unwrap the result.
pub async fn try_run_sql(
    sql: impl Into<String>,
    namespace: impl Into<String>,
    querier_connection: Connection,
) -> Result<Vec<RecordBatch>, influxdb_iox_client::flight::Error> {
    try_run_query(sql, QueryType::Sql, namespace, querier_connection).await
}

/// Runs a SQL query using the flight API on the specified connection.
///
/// Use [`try_run_sql`] if you want to check the error manually.
pub async fn run_sql(
    sql: impl Into<String>,
    namespace: impl Into<String>,
    querier_connection: Connection,
) -> Vec<RecordBatch> {
    try_run_sql(sql, namespace, querier_connection)
        .await
        .expect("Error executing query")
}

/// Runs an InfluxQL query using the flight API on the specified connection.
pub async fn try_run_influxql(
    sql: impl Into<String>,
    namespace: impl Into<String>,
    querier_connection: Connection,
) -> Result<Vec<RecordBatch>, influxdb_iox_client::flight::Error> {
    try_run_query(sql, QueryType::InfluxQl, namespace, querier_connection).await
}

/// Runs an InfluxQL query using the flight API on the specified connection.
///
/// Use [`try_run_influxql`] if you want to check the error manually.
pub async fn run_influxql(
    sql: impl Into<String>,
    namespace: impl Into<String>,
    querier_connection: Connection,
) -> Vec<RecordBatch> {
    try_run_query(sql, QueryType::InfluxQl, namespace, querier_connection)
        .await
        .expect("Error executing query")
}
