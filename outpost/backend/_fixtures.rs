use std::collections::BTreeMap;
use std::fmt::Debug;
use std::future::Future;
use std::path::Path;
use std::sync::{Arc, Mutex};

use tokio::fs;

// tests run in parallel so each file gets a lock to keep a reader from seeing a partial write
static FILE_LOCKS: Mutex<BTreeMap<String, Arc<tokio::sync::Mutex<()>>>> = Mutex::new(BTreeMap::new());

pub(crate) async fn read_or_record<F, Fut, E>(name: &str, fetch: F) -> String
where
	F: FnOnce() -> Fut,
	Fut: Future<Output = Result<String, E>>,
	E: Debug,
{
	let lock = Arc::clone(FILE_LOCKS.lock().unwrap().entry(name.to_owned()).or_default());
	let _guard = lock.lock().await;
	let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("backend/__fixtures").join(name);
	match fs::read_to_string(&file).await {
		Ok(body) => body,
		Err(_) => {
			let body = fetch().await.unwrap();
			fs::create_dir_all(file.parent().unwrap()).await.unwrap();
			fs::write(&file, &body).await.unwrap();
			body
		}
	}
}
