// Copyright 2023 IOP Systems, Inc.
// Licensed under the Apache License, Version 2.0
// http://www.apache.org/licenses/LICENSE-2.0

use super::*;

pub async fn get_requests(
    work_sender: Sender<WorkItem>,
    mut keyspace: Keyspace,
    rate: Option<NonZeroU64>,
) -> Result<()> {
    // if the rate is none, we treat as non-ratelimited and add items to
    // the work queue as quickly as possible
    if rate.is_none() {
        while RUNNING.load(Ordering::Relaxed) {
            let key = keyspace.sample();

            let _ = work_sender.send(WorkItem::Get { key }).await;
        }

        return Ok(());
    }

    let (quanta, mut interval) = convert_ratelimit(rate.unwrap());

    while RUNNING.load(Ordering::Relaxed) {
        interval.tick().await;
        for _ in 0..quanta {
            let key = keyspace.sample();
            let _ = work_sender.send(WorkItem::Get { key }).await;
        }
    }

    Ok(())
}