//! rmus-dl 入口：三种模式按第一个参数分流。
//!
//! - 无参数：交互式 CLI（搜索 → 终端里勾选 → 下载到当前目录）
//! - `--rmus-manifest`：向 rmusd 握手，输出一行能力宣告 JSON 后退出
//! - `--rmus-plugin`：作为 rmusd 的插件子进程长驻，stdin/stdout 走协议
//!
//! 协议细节见 `plugin` 模块文档与 rmus 仓库 `src/plugin.rs`。

fn main() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    match std::env::args().nth(1).as_deref() {
        Some("--rmus-manifest") => {
            println!("{}", rmus_dl::plugin::manifest_json());
            Ok(())
        }
        Some("--rmus-plugin") => runtime.block_on(rmus_dl::plugin::run()),
        _ => runtime.block_on(rmus_dl::app::run()),
    }
}
