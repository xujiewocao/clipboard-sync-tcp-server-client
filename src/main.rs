mod clipboard;
mod network_alternative;
mod notification;

use clipboard::ClipboardManager;
use network_alternative::NetworkManager;
use notification::NotificationManager;
use anyhow::Result;
use clap::{Parser, Subcommand};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "clipboard-sync-alt")]
#[command(about = "跨平台剪贴板同步工具 (TCP直连版本)")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 启动同步服务（作为服务器）
    Start {
        /// 设备名称
        #[arg(short, long, default_value = "我的设备")]
        name: String,
        /// 监听端口
        #[arg(short, long, default_value_t = 8765)]
        port: u16,
    },
    /// 连接到指定设备
    Connect {
        /// 设备名称
        #[arg(short, long, default_value = "我的设备")]
        name: String,
        /// 目标设备IP地址
        ip: String,
        /// 目标设备端口
        #[arg(short, long, default_value_t = 8765)]
        port: u16,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // 初始化剪贴板管理器
    let clipboard = ClipboardManager::new()?;
    
    match cli.command {
        Commands::Start { name, port } => {
            let network = NetworkManager::new(name);
            run_server(clipboard, network, port).await?;
        }
        Commands::Connect { name, ip, port } => {
            let network = NetworkManager::new(name);
            connect_to_server(clipboard, network, &ip, port).await?;
        }
    }

    Ok(())
}

/// 运行服务器模式
async fn run_server(clipboard: ClipboardManager, network: NetworkManager, port: u16) -> Result<()> {
    let notifier = NotificationManager::new();
    
    println!("🚀 启动剪贴板同步服务...");
    
    // 启动网络服务
    network.start_server(port).await?;
    
    // 发送启动通知
    notifier.send("剪贴板同步", "同步服务已启动")?;
    
    // 显示设备信息
    println!("📱 设备名称: {}", network.get_device_name());
    println!("🔌 监听端口: {}", port);
    
    // 获取并显示本地IP地址
    if let Ok(local_ip) = get_local_ip() {
        println!("🌐 本地地址: {}:{}", local_ip, port);
        println!("💡 其他设备可以使用以下命令连接:");
        println!("   cargo run -- connect --name \"设备名称\" {} --port {}", local_ip, port);
    }
    
    println!("");
    println!("📋 监控剪贴板变化中...");
    println!("按 Ctrl+C 停止服务");
    
    // 设置消息处理器
    let mut message_receiver = network.setup_message_handler().await;
    
    // 启动消息处理任务
    let clipboard_clone = clipboard.clone();
    let notifier_clone = notifier.clone();
    tokio::spawn(async move {
        while let Some(message) = message_receiver.recv().await {
            println!("📨 收到剪贴板消息: {} (来自: {})", 
                     message.content.preview(50), 
                     message.sender_name);
            
            // 根据消息类型更新本地剪贴板
            match &message.content {
                network_alternative::ClipboardContent::Text(text) => {
                    if let Err(e) = clipboard_clone.set_text(text) {
                        eprintln!("❌ 更新文本剪贴板失败: {}", e);
                    } else {
                        let preview = message.content.preview(50);
                        let _ = notifier_clone.send("文本剪贴板已同步", &preview);
                    }
                }
                network_alternative::ClipboardContent::Image { width, height, data } => {
                    if let Err(e) = clipboard_clone.set_image(*width, *height, data) {
                        eprintln!("❌ 更新图片剪贴板失败: {}", e);
                    } else {
                        let preview = format!("图片 {}x{}", width, height);
                        let _ = notifier_clone.send("图片剪贴板已同步", &preview);
                    }
                }
            }
        }
    });
    
    // 剪贴板监控循环
    let mut last_text_content = String::new();
    let mut last_content_type = clipboard::ClipboardContentType::Empty;
    
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        
        // 检查剪贴板内容类型
        let current_type = clipboard.get_content_type();
        
        match current_type {
            clipboard::ClipboardContentType::Text => {
                if let Ok(current_content) = clipboard.get_text() {
                    if current_content != last_text_content && !current_content.is_empty() {
                        println!("📋 检测到文本剪贴板变化: {}", current_content);
                        
                        // 广播文本到其他设备
                        if let Err(e) = network.broadcast_clipboard(&current_content).await {
                            eprintln!("❌ 文本广播失败: {}", e);
                        }
                        
                        last_text_content = current_content;
                        last_content_type = current_type;
                    }
                }
            }
            clipboard::ClipboardContentType::Image => {
                // 只有当之前不是图片类型时才处理，避免重复处理
                if !matches!(last_content_type, clipboard::ClipboardContentType::Image) {
                    if let Ok(Some((width, height, png_data))) = clipboard.get_image() {
                        println!("🖼️ 检测到图片剪贴板变化: {}x{}", width, height);
                        
                        // 广播图片到其他设备
                        if let Err(e) = network.broadcast_image(width, height, png_data).await {
                            eprintln!("❌ 图片广播失败: {}", e);
                        }
                        
                        last_content_type = current_type;
                    }
                }
            }
            clipboard::ClipboardContentType::Empty => {
                // 剪贴板为空，更新状态
                if !matches!(last_content_type, clipboard::ClipboardContentType::Empty) {
                    last_content_type = current_type;
                    last_text_content.clear();
                }
            }
        }

        // 检查退出信号
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                break;
            }
            _ = tokio::time::sleep(Duration::from_millis(10)) => {}
        }
    }
    
    network.shutdown().await;
    println!("🔴 同步服务已停止");
    
    Ok(())
}

/// 连接到服务器模式
async fn connect_to_server(clipboard: ClipboardManager, network: NetworkManager, ip: &str, port: u16) -> Result<()> {
    let notifier = NotificationManager::new();
    
    println!("🔗 正在连接到设备: {}:{}", ip, port);
    
    // 连接到指定设备（忽略返回的device_id）
    let _device_id = network.connect_to_device(ip, port).await?;
    
    println!("✅ 连接成功！开始同步剪贴板内容...");
    notifier.send("剪贴板同步", "已连接到设备")?;
    
    // 设置消息处理器
    let mut message_receiver = network.setup_message_handler().await;
    
    // 启动消息处理任务
    let clipboard_clone = clipboard.clone();
    let notifier_clone = notifier.clone();
    tokio::spawn(async move {
        while let Some(message) = message_receiver.recv().await {
            println!("📨 收到剪贴板消息: {} (来自: {})", 
                     message.content.preview(50), 
                     message.sender_name);
            
            // 根据消息类型更新本地剪贴板
            match &message.content {
                network_alternative::ClipboardContent::Text(text) => {
                    if let Err(e) = clipboard_clone.set_text(text) {
                        eprintln!("❌ 更新文本剪贴板失败: {}", e);
                    } else {
                        let preview = message.content.preview(50);
                        let _ = notifier_clone.send("文本剪贴板已同步", &preview);
                    }
                }
                network_alternative::ClipboardContent::Image { width, height, data } => {
                    if let Err(e) = clipboard_clone.set_image(*width, *height, data) {
                        eprintln!("❌ 更新图片剪贴板失败: {}", e);
                    } else {
                        let preview = format!("图片 {}x{}", width, height);
                        let _ = notifier_clone.send("图片剪贴板已同步", &preview);
                    }
                }
            }
        }
    });
    
    println!("📋 监控剪贴板变化中...");
    println!("按 Ctrl+C 断开连接");
    
    // 剪贴板监控循环
    let mut last_text_content = String::new();
    let mut last_content_type = clipboard::ClipboardContentType::Empty;
    
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        
        // 检查剪贴板内容类型
        let current_type = clipboard.get_content_type();
        
        match current_type {
            clipboard::ClipboardContentType::Text => {
                if let Ok(current_content) = clipboard.get_text() {
                    if current_content != last_text_content && !current_content.is_empty() {
                        println!("📋 检测到文本剪贴板变化: {}", current_content);
                        
                        // 广播文本到其他设备
                        if let Err(e) = network.broadcast_clipboard(&current_content).await {
                            eprintln!("❌ 文本广播失败: {}", e);
                        }
                        
                        last_text_content = current_content;
                        last_content_type = current_type;
                    }
                }
            }
            clipboard::ClipboardContentType::Image => {
                // 只有当之前不是图片类型时才处理，避免重复处理
                if !matches!(last_content_type, clipboard::ClipboardContentType::Image) {
                    if let Ok(Some((width, height, png_data))) = clipboard.get_image() {
                        println!("🖼️ 检测到图片剪贴板变化: {}x{}", width, height);
                        
                        // 广播图片到其他设备
                        if let Err(e) = network.broadcast_image(width, height, png_data).await {
                            eprintln!("❌ 图片广播失败: {}", e);
                        }
                        
                        last_content_type = current_type;
                    }
                }
            }
            clipboard::ClipboardContentType::Empty => {
                // 剪贴板为空，更新状态
                if !matches!(last_content_type, clipboard::ClipboardContentType::Empty) {
                    last_content_type = current_type;
                    last_text_content.clear();
                }
            }
        }

        // 检查退出信号
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                break;
            }
        }
    }
    
    network.shutdown().await;
    println!("🔴 连接已断开");
    
    Ok(())
}

/// 获取本地IP地址
fn get_local_ip() -> Result<String> {
    use std::net::{UdpSocket, SocketAddr};
    
    // 创建一个UDP socket连接到外部地址来获取本地IP
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    let dest = SocketAddr::from(([8, 8, 8, 8], 80));
    socket.connect(dest)?;
    let local_addr = socket.local_addr()?;
    Ok(local_addr.ip().to_string())
}
