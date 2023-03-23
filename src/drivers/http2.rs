// SPDX-License-Identifier: (Apache-2.0)
// Copyright Authors of rpc-perf

use super::*;
use crate::net::Connector;
use bytes::Bytes;
use http_body_util::Empty;
use hyper::header::{HeaderName, HeaderValue};
use hyper::{Request, Uri};

/// Launch tasks with one conncetion per task as http/1.1 is not mux'd
pub fn launch_tasks(runtime: &mut Runtime, config: Config, work_receiver: Receiver<WorkItem>) {
    debug!("launching http1 protocol tasks");

    for _ in 0..config.connection().poolsize() {
        for endpoint in config.target().endpoints() {
            runtime.spawn(task(
                work_receiver.clone(),
                endpoint.clone(),
                config.clone(),
            ));
        }
    }
}

// a task for http/1.1
#[allow(clippy::slow_vector_initialization)]
async fn task(work_receiver: Receiver<WorkItem>, endpoint: String, config: Config) -> Result<()> {
    let connector = Connector::new(&config)?;
    let mut sender = None;

    while RUNNING.load(Ordering::Relaxed) {
        if sender.is_none() {
            CONNECT.increment();
            let stream =
                match timeout(config.connection().timeout(), connector.connect(&endpoint)).await {
                    Ok(Ok(s)) => s,
                    Ok(Err(_)) => {
                        CONNECT_EX.increment();
                        sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                    Err(_) => {
                        CONNECT_TIMEOUT.increment();
                        sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };

            let (s, conn) = match hyper::client::conn::http1::handshake(stream).await {
                Ok((s, c)) => (s, c),
                Err(_e) => {
                    CONNECT_EX.increment();
                    sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };

            SESSION.increment();

            sender = Some(s);

            tokio::task::spawn(async move {
                if let Err(err) = conn.await {
                    println!("Connection failed: {:?}", err);
                }
            });
        }

        let mut s = sender.take().unwrap();

        let work_item = work_receiver
            .recv()
            .await
            .map_err(|_| Error::new(ErrorKind::Other, "channel closed"))?;

        REQUEST.increment();

        // compose request into buffer
        let request = match work_item {
            WorkItem::Get { .. } => {
                let url: Uri = format!("http://{endpoint}/").parse().unwrap();
                let authority = url.authority().unwrap().clone();
                Request::builder()
                    .uri(url)
                    .header(hyper::header::HOST, authority.as_str())
                    .body(Empty::<Bytes>::new())
                    .expect("failed to build request")
            }
            WorkItem::Reconnect => {
                SESSION_CLOSED_CLIENT.increment();
                REQUEST_RECONNECT.increment();
                continue;
            }
            _ => {
                REQUEST_UNSUPPORTED.increment();
                sender = Some(s);
                continue;
            }
        };

        REQUEST_OK.increment();

        // send request
        let start = Instant::now();
        let response = timeout(config.request().timeout(), s.send_request(request)).await;
        let stop = Instant::now();

        match response {
            Ok(Ok(response)) => {
                // validate response
                match work_item {
                    WorkItem::Get { .. } => {
                        GET_OK.increment();
                    }
                    _ => {
                        error!("unexpected work item");
                        unimplemented!();
                    }
                }

                RESPONSE_OK.increment();
                RESPONSE_LATENCY.increment(stop, stop.duration_since(start).as_nanos(), 1);

                if let Some(header) = response
                    .headers()
                    .get(HeaderName::from_bytes(b"Connection").unwrap())
                {
                    if header == HeaderValue::from_static("close") {
                        SESSION_CLOSED_SERVER.increment();
                    }
                }
            }
            Ok(Err(_e)) => {
                // record execption
                match work_item {
                    WorkItem::Get { .. } => {
                        GET_EX.increment();
                    }
                    _ => {
                        error!("unexpected work item");
                        unimplemented!();
                    }
                }
                SESSION_CLOSED_CLIENT.increment();
                continue;
            }
            Err(_) => {
                RESPONSE_TIMEOUT.increment();
                SESSION_CLOSED_CLIENT.increment();
                continue;
            }
        }

        if let Err(_e) = s.ready().await {
            continue;
        }

        sender = Some(s);
    }

    Ok(())
}
