use data_types::{NamespaceId, TableId};
use dml::DmlWrite;
use generated_types::influxdata::{
    iox::wal::v1::sequenced_wal_op::Op as WalOp,
    pbdata::v1::{DatabaseBatch, TableBatch},
};
use mutable_batch_lp::lines_to_batches;
use wal::SequencedWalOp;

#[tokio::test]
async fn crud() {
    let dir = test_helpers::tmp_dir().unwrap();

    let wal = wal::Wal::new(dir.path()).await.unwrap();
    let wal_reader = wal.read_handle();

    // Just-created WALs have no closed segments.
    let closed = wal_reader.closed_segments().await;
    assert!(
        closed.is_empty(),
        "Expected empty closed segments; got {:?}",
        closed
    );

    let open = wal.write_handle().await;

    // Can write an entry to the open segment
    let op = arbitrary_sequenced_wal_op(42);
    let summary = open.write_op(op).await.unwrap();
    assert_eq!(summary.total_bytes, 122);
    assert_eq!(summary.bytes_written, 106);

    // Can write another entry; total_bytes accumulates
    let op = arbitrary_sequenced_wal_op(43);
    let summary = open.write_op(op).await.unwrap();
    assert_eq!(summary.total_bytes, 228);
    assert_eq!(summary.bytes_written, 106);

    // Still no closed segments
    let closed = wal_reader.closed_segments().await;
    assert!(
        closed.is_empty(),
        "Expected empty closed segments; got {:?}",
        closed
    );

    // Can't read entries from the open segment; have to rotate first
    let wal_rotator = wal.rotation_handle();
    let closed_segment_details = wal_rotator.rotate().await.unwrap();
    assert_eq!(closed_segment_details.size(), 228);

    // There's one closed segment
    let closed = wal_reader.closed_segments().await;
    let closed_segment_ids: Vec<_> = closed.iter().map(|c| c.id()).collect();
    assert_eq!(closed_segment_ids, &[closed_segment_details.id()]);

    // Can read the written entries from the closed segment
    let mut reader = wal_reader
        .reader_for_segment(closed_segment_details.id())
        .await
        .unwrap();
    let op = reader.next_op().await.unwrap().unwrap();
    assert_eq!(op.sequence_number, 42);
    let op = reader.next_op().await.unwrap().unwrap();
    assert_eq!(op.sequence_number, 43);

    // Can delete a segment, leaving no closed segments again
    wal_rotator
        .delete(closed_segment_details.id())
        .await
        .unwrap();
    let closed = wal_reader.closed_segments().await;
    assert!(
        closed.is_empty(),
        "Expected empty closed segments; got {:?}",
        closed
    );
}

#[tokio::test]
async fn replay() {
    let dir = test_helpers::tmp_dir().unwrap();

    // Create a WAL with an entry, rotate to close the segment, create another entry, then drop the
    // WAL.
    {
        let wal = wal::Wal::new(dir.path()).await.unwrap();
        let open = wal.write_handle().await;
        let op = arbitrary_sequenced_wal_op(42);
        open.write_op(op).await.unwrap();
        let wal_rotator = wal.rotation_handle();
        wal_rotator.rotate().await.unwrap();
        let op = arbitrary_sequenced_wal_op(43);
        open.write_op(op).await.unwrap();
    }

    // Create a new WAL instance with the same directory to replay from the files
    let wal = wal::Wal::new(dir.path()).await.unwrap();
    let wal_reader = wal.read_handle();

    // There's two closed segments -- one for the previously closed segment, one for the previously
    // open segment. Replayed WALs treat all files as closed, because effectively they are.
    let closed = wal_reader.closed_segments().await;
    let closed_segment_ids: Vec<_> = closed.iter().map(|c| c.id()).collect();
    assert_eq!(closed_segment_ids.len(), 2);

    // Can read the written entries from the previously closed segment
    let mut reader = wal_reader
        .reader_for_segment(closed_segment_ids[0])
        .await
        .unwrap();
    let op = reader.next_op().await.unwrap().unwrap();
    assert_eq!(op.sequence_number, 42);

    // Can read the written entries from the previously open segment
    let mut reader = wal_reader
        .reader_for_segment(closed_segment_ids[1])
        .await
        .unwrap();
    let op = reader.next_op().await.unwrap().unwrap();
    assert_eq!(op.sequence_number, 43);
}

#[tokio::test]
async fn ordering() {
    let dir = test_helpers::tmp_dir().unwrap();

    // Create a WAL with two closed segments and an open segment with entries, then drop the WAL
    {
        let wal = wal::Wal::new(dir.path()).await.unwrap();
        let open = wal.write_handle().await;
        let wal_rotator = wal.rotation_handle();

        let op = arbitrary_sequenced_wal_op(42);
        open.write_op(op).await.unwrap();
        wal_rotator.rotate().await.unwrap();

        let op = arbitrary_sequenced_wal_op(43);
        open.write_op(op).await.unwrap();
        wal_rotator.rotate().await.unwrap();

        let op = arbitrary_sequenced_wal_op(44);
        open.write_op(op).await.unwrap();
    }

    // Create a new WAL instance with the same directory to replay from the files
    let wal = wal::Wal::new(dir.path()).await.unwrap();
    let wal_reader = wal.read_handle();
    let wal_rotator = wal.rotation_handle();

    // There are 3 segments (from the 2 closed and 1 open) and they're in the order they were
    // created
    let closed = wal_reader.closed_segments().await;
    let closed_segment_ids: Vec<_> = closed.iter().map(|c| c.id().get()).collect();
    assert_eq!(closed_segment_ids, &[0, 1, 2]);

    // The open segment is next in order
    let closed_segment_details = wal_rotator.rotate().await.unwrap();
    assert_eq!(closed_segment_details.id().get(), 3);

    // Creating new files after replay are later in the ordering
    let closed_segment_details = wal_rotator.rotate().await.unwrap();
    assert_eq!(closed_segment_details.id().get(), 4);
}

fn arbitrary_sequenced_wal_op(sequence_number: u64) -> SequencedWalOp {
    let w = test_data("m1,t=foo v=1i 1");
    SequencedWalOp {
        sequence_number,
        op: WalOp::Write(w),
    }
}

fn test_data(lp: &str) -> DatabaseBatch {
    let batches = lines_to_batches(lp, 0).unwrap();
    let batches = batches
        .into_iter()
        .enumerate()
        .map(|(i, (_table_name, batch))| (TableId::new(i as _), batch))
        .collect();

    let write = DmlWrite::new(
        NamespaceId::new(42),
        batches,
        "bananas".into(),
        Default::default(),
    );

    let database_batch = mutable_batch_pb::encode::encode_write(42, &write);

    // `encode_write` returns tables and columns in an arbitrary order. Sort tables and columns
    // to make tests deterministic.
    let DatabaseBatch {
        database_id,
        partition_key,
        table_batches,
    } = database_batch;

    let mut table_batches: Vec<_> = table_batches
        .into_iter()
        .map(|table_batch| {
            let TableBatch {
                table_id,
                mut columns,
                row_count,
            } = table_batch;

            columns.sort_by(|a, b| a.column_name.cmp(&b.column_name));

            TableBatch {
                table_id,
                columns,
                row_count,
            }
        })
        .collect();

    table_batches.sort_by_key(|t| t.table_id);

    DatabaseBatch {
        database_id,
        partition_key,
        table_batches,
    }
}
