use crate::file_receiver::start_file_receiver;
use bevy::prelude::*;
use std::path::PathBuf;
use tokio::runtime::Runtime;

// 网络插件
pub struct NetworkPlugin;

impl Plugin for NetworkPlugin {
    fn build(&self, app: &mut App) {
        // 创建一个并行的 Tokio Runtime 并作为 Resource 存入 Bevy
        // 这样插件内部和用户都能访问到同一个 Runtime
        // 使用 Builder 模式可以更好地控制线程数（移动端优化）
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to create Tokio runtime");

        app.insert_resource(TokioRuntime(runtime));
        app.add_systems(Startup, setup_network_listener);
    }
}

fn setup_network_listener(runtime: Res<TokioRuntime>, // 注入我们存好的运行时
) {
    // 拿到 handle，它是轻量级可克隆的
    let handle = runtime.0.handle().clone();
    let save_path = PathBuf::from("./downloads");

    // 使用 tokio 的 spawn，它会自动关联 Reactor
    handle.clone().spawn(async move {
        // 确保下载目录存在
        let _ = tokio::fs::create_dir_all(&save_path).await;

        let listener = match tokio::net::TcpListener::bind("0.0.0.0:49527").await {
            Ok(l) => l,
            Err(e) => {
                error!("❌ 无法绑定端口 49527: {}", e);
                return;
            }
        };
        info!("👂 插件监听已启动: 49527");

        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    info!("📥 收到来自 {} 的连接请求", addr);
                    let path = save_path.clone();

                    // 关键：必须使用 handle.spawn 来启动每一个文件接收任务
                    // 这样即使主监听循环在忙，接收任务也能在线程池中并行处理
                    handle.spawn(async move {
                        if let Err(e) = start_file_receiver(stream, path).await {
                            error!("❌ 接收文件时出错 (来自 {}): {:?}", addr, e);
                        }
                    });
                }
                Err(e) => error!("⚠️ TCP Accept Error: {}", e),
            }
        }
    });
}

#[derive(Resource)]
pub struct TokioRuntime(pub Runtime);
