use chrono::{Duration, Utc};
use hyuqueue_core::{
  event::{Actor, EventType, Locality},
  item::Item,
  queue as queue_names,
};
use hyuqueue_store::{events, items, queue, Db};
use serde_json::json;
use uuid::Uuid;

async fn test_db() -> Db {
  Db::open(":memory:").await.unwrap()
}

fn test_item(source: &str) -> Item {
  let now = Utc::now();
  Item {
    id: Uuid::new_v4(),
    title: format!("Test item ({source})"),
    body: Some("Test body".to_string()),
    source_topic_id: None,
    source: source.to_string(),
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
  assert_eq!(fetched.source, "test");
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
