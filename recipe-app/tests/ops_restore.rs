use image::{DynamicImage, ImageFormat, Rgb};
use serde_json::{json, Value};
use std::{
    io::Cursor,
    net::{SocketAddr, TcpListener},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;

const TOKEN: &str = "ops-restore-test-secret";

struct RunningApp {
    child: Child,
    address: SocketAddr,
}

impl RunningApp {
    async fn start(directory: &Path) -> Self {
        std::fs::create_dir_all(directory).unwrap();
        let address = unused_address();
        let config = directory.join("config.toml");
        std::fs::write(
            &config,
            format!(
                "bind_addr = {:?}\ndatabase_path = {:?}\ncontent_dir = {:?}\napi_tokens = [\"ops:{}\"]\n",
                address.to_string(),
                directory.join("recipes.sqlite3").display().to_string(),
                directory.join("content").display().to_string(),
                TOKEN,
            ),
        )
        .unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_recipe-app"))
            .env("APP_CONFIG_FILE", config)
            .env_remove("APP__BIND_ADDR")
            .env_remove("APP__DATABASE_PATH")
            .env_remove("APP__CONTENT_DIR")
            .env_remove("APP__API_TOKENS")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start actual recipe-app binary");
        let app = Self { child, address };
        app.wait_healthy().await;
        app
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }

    async fn wait_healthy(&self) {
        let client = reqwest::Client::new();
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if let Ok(response) = client
                .get(format!("{}/health", self.base_url()))
                .send()
                .await
            {
                if response.status().is_success() {
                    return;
                }
            }
            thread::sleep(Duration::from_millis(50));
        }
        panic!("recipe-app did not become healthy");
    }

    fn stop(mut self) {
        self.child.kill().expect("stop source recipe-app");
        self.child.wait().expect("wait for source recipe-app");
    }
}

impl Drop for RunningApp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn unused_address() -> SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.local_addr().unwrap()
}

fn png(color: [u8; 3]) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(image::RgbImage::from_pixel(9, 7, Rgb(color)))
        .write_to(&mut bytes, ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

async fn upload_and_attach(
    client: &reqwest::Client,
    base: &str,
    recipe: &Value,
    bytes: Vec<u8>,
) -> (Value, Vec<u8>) {
    let uploaded = client
        .post(format!("{base}/api/v1/images"))
        .bearer_auth(TOKEN)
        .header("content-type", "image/png")
        .body(bytes)
        .send()
        .await
        .unwrap();
    assert_eq!(uploaded.status(), 201);
    let image: Value = uploaded.json().await.unwrap();
    let attached = client
        .post(format!(
            "{base}/api/v1/recipes/{}/image",
            recipe["id"].as_str().unwrap()
        ))
        .bearer_auth(TOKEN)
        .json(&json!({"base_revision":recipe["revision"],"image_ref":image["ref"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(attached.status(), 200);
    let attached: Value = attached.json().await.unwrap();
    let raw = client
        .get(format!("{base}{}", image["url"].as_str().unwrap()))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap()
        .to_vec();
    assert_eq!(attached["image"]["ref"], image["ref"]);
    (attached, raw)
}

#[tokio::test]
async fn documented_backup_restore_recovers_actual_app_history_and_image_bytes() {
    let root = TempDir::new().unwrap();
    let live = root.path().join("live");
    let backup = root.path().join("backup");
    let restored = root.path().join("restored");
    let source_app = RunningApp::start(&live).await;
    let client = reqwest::Client::new();
    let base = source_app.base_url();

    let created = client
        .post(format!("{base}/api/v1/recipes"))
        .bearer_auth(TOKEN)
        .json(&json!({"title":"Backup test soup","servings":2,"body":"@salt{1%g}"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let created: Value = created.json().await.unwrap();
    let recipe = created["recipe"].clone();
    let (historical, old_image_bytes) =
        upload_and_attach(&client, &base, &recipe, png([210, 20, 30])).await;
    let (current, current_image_bytes) =
        upload_and_attach(&client, &base, &historical, png([20, 30, 210])).await;
    assert_ne!(old_image_bytes, current_image_bytes);

    let revisions: Vec<Value> = client
        .get(format!(
            "{base}/api/v1/recipes/{}/revisions",
            recipe["id"].as_str().unwrap()
        ))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(revisions.len(), 3);
    assert_eq!(revisions[1]["image"]["ref"], historical["image"]["ref"]);
    assert_eq!(revisions[2]["image"]["ref"], current["image"]["ref"]);

    source_app.stop();
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../scripts/ops.py");
    let output = Command::new("python3")
        .arg(&script)
        .args(["backup", "--database"])
        .arg(live.join("recipes.sqlite3"))
        .arg("--content")
        .arg(live.join("content"))
        .arg("--output")
        .arg(&backup)
        .output()
        .expect("run documented backup CLI");
    assert!(
        output.status.success(),
        "backup failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = Command::new("python3")
        .arg(&script)
        .args(["restore", "--backup"])
        .arg(&backup)
        .arg("--destination")
        .arg(&restored)
        .output()
        .expect("run documented restore CLI");
    assert!(
        output.status.success(),
        "restore failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let restored_app = RunningApp::start(&restored).await;
    let restored_base = restored_app.base_url();
    let restored_current: Value = client
        .get(format!(
            "{restored_base}/api/v1/recipes/{}",
            recipe["id"].as_str().unwrap()
        ))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(restored_current["revision"], current["revision"]);
    assert_eq!(restored_current["image"]["ref"], current["image"]["ref"]);
    let restored_revisions: Vec<Value> = client
        .get(format!(
            "{restored_base}/api/v1/recipes/{}/revisions",
            recipe["id"].as_str().unwrap()
        ))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(restored_revisions, revisions);
    let old_reference = historical["image"]["ref"].as_str().unwrap();
    let new_reference = current["image"]["ref"].as_str().unwrap();
    for (reference, expected) in [
        (old_reference, old_image_bytes),
        (new_reference, current_image_bytes),
    ] {
        let actual = client
            .get(format!("{restored_base}/images/{reference}"))
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        assert_eq!(actual.as_ref(), expected);
    }
}
