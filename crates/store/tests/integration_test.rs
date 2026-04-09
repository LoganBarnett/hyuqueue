use chrono::Utc;
use hyuqueue_core::{
  event::{Actor, EventType, Locality},
  item::{Item, ItemState},
  queue::Queue,
};
use hyuqueue_store::{events, items, queues, Db};
use serde_json::json;
use uuid::Uuid;

async fn test_db() -> Db {
  Db::open(":memory:").await.unwrap()
}

fn test_queue() -> Queue {
  let now = Utc::now();
  Queue {
    id: Uuid::new_v4(),
    name: "test-queue".to_string(),
    tags: vec!["test".to_string()],
    config: json!({}),
    created_at: now,
    updated_at: now,
  }
}

fn test_item(queue_id: Uuid) -> Item {
  let now = Utc::now();
  Item {
    id: Uuid::new_v4(),
    queue_id,
    title: "Test item".to_string(),
    body: Some("Test body".to_string()),
    source_topic_id: None,
    source: "test".to_string(),
    delegate_from: None,
    delegate_chain: vec![],
    capabilities: vec![],
    metadata: json!({}),
    state: ItemState::IntakePending,
    created_at: now,
    updated_at: now,
  }
}

#[tokio::test]
async fn test_queue_crud() {
  let db = test_db().await;
  let queue = test_queue();

  queues::insert(&db, &queue).await.unwrap();

  let all = queues::list(&db).await.unwrap();
  assert_eq!(all.len(), 1);
  assert_eq!(all[0].name, "test-queue");
}

#[tokio::test]
async fn test_item_lifecycle() {
  let db = test_db().await;
  let queue = test_queue();
  queues::insert(&db, &queue).await.unwrap();

  let item = test_item(queue.id);
  items::insert(&db, &item).await.unwrap();

  // Verify item exists.
  let fetched = items::get(&db, item.id).await.unwrap();
  assert_eq!(fetched.title, "Test item");
  assert_eq!(fetched.state, ItemState::IntakePending);

  // Transition to HumanPending.
  items::update_state(&db, item.id, ItemState::HumanPending)
    .await
    .unwrap();
  let fetched = items::get(&db, item.id).await.unwrap();
  assert_eq!(fetched.state, ItemState::HumanPending);

  // Should appear as next item.
  let next = items::next_human_item(&db).await.unwrap();
  assert!(next.is_some());
  assert_eq!(next.unwrap().id, item.id);

  // Count should be 1.
  let count = items::human_queue_count(&db).await.unwrap();
  assert_eq!(count, 1);

  // Ack (transition to Done).
  items::update_state(&db, item.id, ItemState::Done)
    .await
    .unwrap();
  let fetched = items::get(&db, item.id).await.unwrap();
  assert_eq!(fetched.state, ItemState::Done);

  // Queue should be empty now.
  let next = items::next_human_item(&db).await.unwrap();
  assert!(next.is_none());
}

#[tokio::test]
async fn test_event_append_and_query() {
  let db = test_db().await;
  let queue = test_queue();
  queues::insert(&db, &queue).await.unwrap();

  let item = test_item(queue.id);
  items::insert(&db, &item).await.unwrap();

  // Append an event.
  let event = events::new_item_event(
    item.id,
    EventType::ItemCreated,
    Actor::System,
    Locality::Local,
    json!({ "source": "test" }),
  );
  events::append(&db, &event).await.unwrap();

  // Query events for item (returns JSON values, not typed events).
  let item_events = events::for_item(&db, item.id).await.unwrap();
  assert_eq!(item_events.len(), 1);
}

#[tokio::test]
async fn test_item_list_filtering() {
  let db = test_db().await;
  let queue = test_queue();
  queues::insert(&db, &queue).await.unwrap();

  // Create items in different states.
  let item1 = test_item(queue.id);
  items::insert(&db, &item1).await.unwrap();

  let item2 = test_item(queue.id);
  items::insert(&db, &item2).await.unwrap();
  items::update_state(&db, item2.id, ItemState::HumanPending)
    .await
    .unwrap();

  // List all.
  let all = items::list(&db, None, None, 50, 0).await.unwrap();
  assert_eq!(all.len(), 2);

  // Filter by state.
  let pending = items::list(&db, None, Some(ItemState::IntakePending), 50, 0)
    .await
    .unwrap();
  assert_eq!(pending.len(), 1);

  // Filter by queue.
  let by_queue = items::list(&db, Some(queue.id), None, 50, 0).await.unwrap();
  assert_eq!(by_queue.len(), 2);
}
