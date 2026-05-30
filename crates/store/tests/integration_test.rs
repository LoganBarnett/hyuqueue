use chrono::{Duration, Utc};
use hyuqueue_core::{
  event::{Actor, EventType, Locality},
  item::Item,
  queue as queue_names,
};
use hyuqueue_store::{events, items, items::ItemsError, queue, Db};
use serde_json::json;
use uuid::Uuid;

async fn test_db() -> Db {
  Db::open(":memory:").await.unwrap()
}

fn test_item(source_instance_id: &str) -> Item {
  let now = Utc::now();
  Item {
    id: Uuid::new_v4(),
    title: format!("Test item ({source_instance_id})"),
    body: Some("Test body".to_string()),
    source_instance_id: Some(source_instance_id.to_string()),
    external_id: None,
    delegate_from: None,
    delegate_chain: vec![],
    capabilities: vec![],
    metadata: json!({}),
    created_at: now,
    updated_at: now,
  }
}

#[tokio::test]
async fn item_insert_and_get() {
  let db = test_db().await;
  let item = test_item("test");
  items::insert(db.pool(), &item).await.unwrap();

  let fetched = items::get(db.pool(), item.id).await.unwrap();
  assert_eq!(fetched.title, item.title);
  assert_eq!(fetched.source_instance_id.as_deref(), Some("test"));
}

#[tokio::test]
async fn event_append_and_query() {
  let db = test_db().await;
  let item = test_item("test");
  items::insert(db.pool(), &item).await.unwrap();

  let event = events::new_item_event(
    item.id,
    EventType::ItemCreated,
    Actor::System,
    Locality::Local,
    json!({ "source": "test" }),
  );
  events::append(db.pool(), &event).await.unwrap();

  let item_events = events::for_item(db.pool(), item.id).await.unwrap();
  assert_eq!(item_events.len(), 1);
}

#[tokio::test]
async fn item_list_with_source_filter() {
  let db = test_db().await;
  let email_item = test_item("email");
  let jira_item = test_item("jira");
  items::insert(db.pool(), &email_item).await.unwrap();
  items::insert(db.pool(), &jira_item).await.unwrap();

  let all = items::list(db.pool(), None, 50, 0).await.unwrap();
  assert_eq!(all.len(), 2);

  let just_email = items::list(db.pool(), Some("email"), 50, 0).await.unwrap();
  assert_eq!(just_email.len(), 1);
  assert_eq!(just_email[0].id, email_item.id);
}

/// Models the scenario from `topic-example`: a topic emits an item
/// with a synthesized `external_id` ("tick-1"), crashes (or its
/// subprocess exits), and on restart re-emits the same item because
/// its in-memory counter reset.  The host-side dedupe constraint
/// should drop the duplicate at insert time rather than letting it
/// into the queue.
#[tokio::test]
async fn duplicate_source_instance_external_id_is_rejected() {
  let db = test_db().await;

  let now = Utc::now();
  let first = Item {
    id: Uuid::new_v4(),
    title: "tick #1".to_string(),
    body: None,
    source_instance_id: Some("example".to_string()),
    external_id: Some("tick-1".to_string()),
    delegate_from: None,
    delegate_chain: vec![],
    capabilities: vec![],
    metadata: json!({}),
    created_at: now,
    updated_at: now,
  };
  items::insert(db.pool(), &first).await.unwrap();

  // Simulate a re-emission after restart: same (instance,
  // external_id) pair, fresh internal UUID.
  let second = Item {
    id: Uuid::new_v4(),
    ..first.clone()
  };
  let err = items::insert(db.pool(), &second).await.unwrap_err();
  match err {
    ItemsError::DuplicateSource {
      source_instance_id,
      external_id,
    } => {
      assert_eq!(source_instance_id.as_deref(), Some("example"));
      assert_eq!(external_id.as_deref(), Some("tick-1"));
    }
    other => panic!("expected DuplicateSource, got {other:?}"),
  }

  // Distinct external_id under the same instance succeeds.
  let other_tick = Item {
    id: Uuid::new_v4(),
    external_id: Some("tick-2".to_string()),
    ..first.clone()
  };
  items::insert(db.pool(), &other_tick).await.unwrap();

  // NULL external_ids are distinct under SQLite's default UNIQUE
  // semantics, so a topic that does not populate external_id (or a
  // one-off pushed item) is *not* subject to dedupe.
  let anon_a = Item {
    id: Uuid::new_v4(),
    external_id: None,
    ..first.clone()
  };
  let anon_b = Item {
    id: Uuid::new_v4(),
    external_id: None,
    ..first.clone()
  };
  items::insert(db.pool(), &anon_a).await.unwrap();
  items::insert(db.pool(), &anon_b).await.unwrap();

  // Final inventory: tick-1, tick-2, anon_a, anon_b.  The duplicate
  // re-emission of tick-1 was rejected.
  let all = items::list(db.pool(), Some("example"), 50, 0)
    .await
    .unwrap();
  assert_eq!(all.len(), 4);
}

#[tokio::test]
async fn item_lifecycle_via_queue_transitions() {
  let db = test_db().await;
  let item = test_item("test");
  items::insert(db.pool(), &item).await.unwrap();

  // Items flow through the system via queue transitions.  Start on
  // the intake queue, get picked up by intake-worker, escalate to
  // human, get acked into outtake, and finally complete.
  queue::enqueue(db.pool(), queue_names::INTAKE, item.id, 0)
    .await
    .unwrap();
  assert_eq!(queue::depth(db.pool(), queue_names::INTAKE).await.unwrap(), 1);

  let intake_worker = "intake-test";
  let entry = queue::dequeue_one(
    &db,
    queue_names::INTAKE,
    intake_worker,
    Duration::seconds(30),
  )
  .await
  .unwrap()
  .expect("expected an item");
  assert_eq!(entry.item_id, item.id);

  // Intake escalates to human.
  queue::move_item_one(
    &db,
    item.id,
    queue_names::INTAKE,
    queue_names::HUMAN,
    intake_worker,
  )
  .await
  .unwrap();
  assert_eq!(queue::depth(db.pool(), queue_names::INTAKE).await.unwrap(), 0);
  assert_eq!(queue::depth(db.pool(), queue_names::HUMAN).await.unwrap(), 1);

  // Human "worker" (the operator's client) takes it, then acks by
  // moving to outtake.
  let human_worker = "human-client";
  let _ = queue::dequeue_one(
    &db,
    queue_names::HUMAN,
    human_worker,
    Duration::minutes(5),
  )
  .await
  .unwrap()
  .expect("human picks up the item");
  queue::move_item_one(
    &db,
    item.id,
    queue_names::HUMAN,
    queue_names::OUTTAKE,
    human_worker,
  )
  .await
  .unwrap();

  // Outtake worker processes and completes.
  let outtake_worker = "outtake-test";
  let _ = queue::dequeue_one(
    &db,
    queue_names::OUTTAKE,
    outtake_worker,
    Duration::minutes(5),
  )
  .await
  .unwrap()
  .expect("outtake picks up the item");
  queue::complete(db.pool(), queue_names::OUTTAKE, item.id, outtake_worker)
    .await
    .unwrap();

  // All queues are empty.
  assert_eq!(queue::depth(db.pool(), queue_names::OUTTAKE).await.unwrap(), 0);
}
