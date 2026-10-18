//! tegata とプロバイダーの約束
//!
//! tegata は、1 回の操作ごとにプロバイダーの実行ファイルを起動し、標準入力に [`Request`] を 1 つ渡し、
//! 標準出力から [`Response`] を 1 つ受け取る。標準エラー出力は運営者に見せる（値を出さない）。
//! プロバイダーは [`Provider`] を実装して [`run`] を呼べばよい

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 約束の版。変わったら上げる
pub const PROTOCOL: u32 = 1;

/// 操作
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    /// 型ごとの種類を返す
    Describe,
    /// 新しく作り、出力と、あとで消すための state を返す
    Create,
    /// state を使って消す
    Delete,
    /// 読むだけ（data）
    Read,
}

/// 型の種類
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// 作って消す（create・delete）
    Managed,
    /// 読むだけ（read）
    Data,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub protocol: u32,
    pub op: Op,
    /// リソースの型（describe では空）
    #[serde(default, rename = "type")]
    pub kind: String,
    /// YAML のリソースの config
    #[serde(default)]
    pub config: serde_json::Value,
    /// YAML のプロバイダーの config（アカウントなど）
    #[serde(default)]
    pub provider_config: serde_json::Value,
    /// 鍵（環境変数か、入力を隠して聞いたもの）。ログに出さない
    #[serde(default)]
    pub credentials: BTreeMap<String, String>,
    /// delete のとき、create が返したもの
    #[serde(default)]
    pub state: serde_json::Value,
    /// delete（prune）のとき、残す最新の世代の state（なければ null）。値が 1 つしか持てないもの（Tunnel の秘密など）で、
    /// 古いものが使われていないかを、入れ替えた時刻と比べて確かめるのに使う。古いプロバイダーは読まずに無視してよい
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub current_state: serde_json::Value,
    /// YAML のファイルのあるディレクトリ（config の相対パスの基準）
    #[serde(default)]
    pub base_dir: String,
    /// リソースの名前（ログのため）
    #[serde(default)]
    pub resource: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Response {
    /// describe：型 → 種類
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub types: BTreeMap<String, Kind>,
    /// describe：作ると前の値がすぐ効かなくなる型（値を 1 つしか持てないもの）。tegata は作る前に運営者に確かめる。
    /// 古いプロバイダーは返さない
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replaces: Vec<String>,
    /// create・read：出力の名前 → 値
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, String>,
    /// create：あとで消すのに要るもの（トークンの ID など。秘密を入れない）
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub state: serde_json::Value,
    /// 失敗したときの理由
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// create の結果
#[derive(Debug, Clone, Default)]
pub struct Created {
    pub outputs: BTreeMap<String, String>,
    pub state: serde_json::Value,
}

impl Request {
    /// config の文字列の項目（なければ失敗）
    pub fn config_str(&self, key: &str) -> anyhow::Result<&str> {
        self.config
            .get(key)
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("config.{key} is required"))
    }

    /// プロバイダーの config の文字列の項目（なければ失敗）
    pub fn provider_str(&self, key: &str) -> anyhow::Result<&str> {
        self.provider_config
            .get(key)
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("provider config.{key} is required"))
    }

    /// 鍵（なければ失敗）
    pub fn credential(&self, key: &str) -> anyhow::Result<&str> {
        self.credentials
            .get(key)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow::anyhow!("credential {key} is required"))
    }
}

/// プロバイダーの実装
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    /// 型 → 種類
    fn describe(&self) -> BTreeMap<String, Kind>;
    /// 作ると前の値がすぐ効かなくなる型（[`Response::replaces`]）
    fn replaces(&self) -> Vec<String> {
        Vec::new()
    }
    async fn create(&self, _req: &Request) -> anyhow::Result<Created> {
        anyhow::bail!("create is not supported")
    }
    async fn delete(&self, _req: &Request) -> anyhow::Result<()> {
        anyhow::bail!("delete is not supported")
    }
    async fn read(&self, _req: &Request) -> anyhow::Result<BTreeMap<String, String>> {
        anyhow::bail!("read is not supported")
    }
}

/// 標準入力の要求を 1 つ処理して、標準出力に応答を書く。プロバイダーの main から呼ぶ
pub fn run<P: Provider>(provider: P) -> ! {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime");
    let (response, ok) = rt.block_on(async {
        use tokio::io::AsyncReadExt;
        let mut input = String::new();
        if let Err(e) = tokio::io::stdin().read_to_string(&mut input).await {
            return failure(format!("cannot read the request: {e}"));
        }
        let req: Request = match serde_json::from_str(&input) {
            Ok(r) => r,
            Err(e) => return failure(format!("invalid request: {e}")),
        };
        if req.protocol != PROTOCOL {
            return failure(format!(
                "protocol {} is not supported (this provider speaks {PROTOCOL})",
                req.protocol
            ));
        }
        if req.op != Op::Describe && !provider.describe().contains_key(&req.kind) {
            return failure(format!("unknown type: {}", req.kind));
        }
        let result = match req.op {
            Op::Describe => Ok(Response {
                types: provider.describe(),
                replaces: provider.replaces(),
                ..Default::default()
            }),
            Op::Create => provider.create(&req).await.map(|c| Response {
                outputs: c.outputs,
                state: c.state,
                ..Default::default()
            }),
            Op::Delete => provider.delete(&req).await.map(|()| Response::default()),
            Op::Read => provider.read(&req).await.map(|outputs| Response {
                outputs,
                ..Default::default()
            }),
        };
        match result {
            Ok(r) => (r, true),
            Err(e) => failure(format!("{e:#}")),
        }
    });
    println!(
        "{}",
        serde_json::to_string(&response).expect("a response serializes")
    );
    std::process::exit(if ok { 0 } else { 1 })
}

fn failure(message: String) -> (Response, bool) {
    (
        Response {
            error: Some(message),
            ..Default::default()
        },
        false,
    )
}
