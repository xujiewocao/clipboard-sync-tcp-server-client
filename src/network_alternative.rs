use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, Mutex};
use tokio::net::{TcpListener as TokioTcpListener, TcpStream as TokioTcpStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// 网络配置常量
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);
const MESSAGE_MAX_SIZE: usize = 10 * 1024 * 1024; // 10MB最大消息大小

/// 剪贴板同步内容
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClipboardContent {
    Text(String),
    Image { width: u32, height: u32, data: Vec<u8> },
}

impl ClipboardContent {
    /// 获取内容预览
    pub fn preview(&self, max_length: usize) -> String {
        match self {
            ClipboardContent::Text(text) => {
                if text.chars().count() > max_length {
                    let truncated: String = text.chars().take(max_length).collect();
                    format!("{}...", truncated)
                } else {
                    text.clone()
                }
            }
            ClipboardContent::Image { width, height, .. } => {
                format!("图片 {}x{}", width, height)
            }
        }
    }
}

/// 剪贴板同步消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardMessage {
    pub content: ClipboardContent,
    pub timestamp: u64,
    pub sender_id: String,
    pub sender_name: String,
}

impl ClipboardMessage {
    /// 创建文本消息
    pub fn new_text(content: String, sender_id: String, sender_name: String) -> Self {
        Self {
            content: ClipboardContent::Text(content),
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            sender_id,
            sender_name,
        }
    }

    /// 创建图片消息
    pub fn new_image(width: u32, height: u32, data: Vec<u8>, sender_id: String, sender_name: String) -> Self {
        Self {
            content: ClipboardContent::Image { width, height, data },
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            sender_id,
            sender_name,
        }
    }

    /// 序列化为字节
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(self).map_err(Into::into)
    }

    /// 从字节反序列化
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        serde_json::from_slice(data).map_err(Into::into)
    }
}

/// 网络管理器
#[derive(Clone)]
pub struct NetworkManager {
    device_name: String,
    connections: Arc<Mutex<HashMap<String, TokioTcpStream>>>,
    message_sender: Arc<Mutex<Option<mpsc::UnboundedSender<ClipboardMessage>>>>,
    is_running: Arc<Mutex<bool>>,
}

impl NetworkManager {
    /// 创建新的网络管理器
    pub fn new(device_name: String) -> Self {
        println!("🌐 启动网络通信服务...");
        
        println!("📱 设备名称: {}", device_name);
        
        Self {
            device_name,
            connections: Arc::new(Mutex::new(HashMap::new())),
            message_sender: Arc::new(Mutex::new(None)),
            is_running: Arc::new(Mutex::new(false)),
        }
    }

    /// 设置消息处理器
    pub async fn setup_message_handler(&self) -> mpsc::UnboundedReceiver<ClipboardMessage> {
        let (sender, receiver) = mpsc::unbounded_channel();
        *self.message_sender.lock().await = Some(sender);
        receiver
    }

    /// 启动网络服务（作为服务器监听连接）
    pub async fn start_server(&self, port: u16) -> Result<()> {
        *self.is_running.lock().await = true;
        
        // 启动TCP数据服务器
        self.start_data_server(port).await?;
        
        println!("✅ 网络服务启动完成，监听端口: {}", port);
        Ok(())
    }

    /// 启动TCP数据服务器
    async fn start_data_server(&self, port: u16) -> Result<()> {
        let listener = TokioTcpListener::bind(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            port,
        )).await?;
        
        println!("🔄 TCP数据服务器启动在端口 {}", port);
        
        let message_sender = self.message_sender.clone();
        let device_name = self.device_name.clone();
        let is_running = self.is_running.clone();
        let connections = self.connections.clone();
        
        tokio::spawn(async move {
            while *is_running.lock().await {
                match listener.accept().await {
                    Ok((stream, addr)) => {
                        println!("📥 接受来自 {} 的连接", addr);
                        
                        let message_sender = message_sender.clone();
                        let device_name = device_name.clone();
                        let connections = connections.clone();
                        
                        // 为每个连接生成一个唯一标识符
                        let device_id = format!("client_{}", addr);
                        
                        // 将连接保存到服务器的连接池中
                        connections.lock().await.insert(device_id.clone(), stream);

                        println!("✅ 添加与 {} 的连接", device_id);
                        println!("connections len: {}", connections.lock().await.len());
                        
                        // 从连接池中获取连接的可变引用
                        if let Some(stream) = connections.lock().await.get_mut(&device_id) {
                            let _ = Self::handle_tcp_connection(stream, message_sender, device_name).await;
                        }
                        
                        // 删除连接
                        connections.lock().await.remove(&device_id);
                        println!("📤 断开与 {} 的连接", addr);
                    }
                    Err(e) => {
                        eprintln!("❌ 接受连接失败: {}", e);
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
        });
        
        Ok(())
    }

    /// 从连接中读取消息
    async fn read_message(
        stream: &mut TokioTcpStream,
        message_sender: Arc<Mutex<Option<mpsc::UnboundedSender<ClipboardMessage>>>>,
        device_name: String,
    ) -> Result<()> {
        // 读取消息长度（4字节）
        let mut len_buf = [0u8; 4];
        match stream.read_exact(&mut len_buf).await {
            Ok(_) => {},
            Err(_) => return Err(anyhow::anyhow!("连接断开")), // 连接断开
        }
        
        let message_len = u32::from_be_bytes(len_buf) as usize;
        if message_len > MESSAGE_MAX_SIZE {
            return Err(anyhow::anyhow!("消息过大: {} bytes", message_len));
        }
        
        // 读取消息内容
        let mut buffer = vec![0u8; message_len];
        stream.read_exact(&mut buffer).await?;
        
        match ClipboardMessage::from_bytes(&buffer) {
            Ok(message) => {
                println!("📨 收到消息: {} (来自: {})", 
                         message.content.preview(50), 
                         message.sender_name);
                
                // 转发消息给处理器
                if let Some(sender) = message_sender.lock().await.as_ref() {
                    if let Err(e) = sender.send(message) {
                        eprintln!("❌ 转发消息失败: {}", e);
                    }
                }
                Ok(())
            }
            Err(e) => {
                Err(anyhow::anyhow!("解析消息失败: {}", e))
            }
        }
    }

    /// 连接到指定设备
    pub async fn connect_to_device(&self, ip: &str, port: u16) -> Result<String> {
        let ip_addr: IpAddr = ip.parse().map_err(|e| anyhow::anyhow!("无效的IP地址: {}", e))?;
        let addr = SocketAddr::new(ip_addr, port);
        
        println!("🔗 正在连接到设备: {}:{}", ip, port);
        
        match tokio::time::timeout(CONNECTION_TIMEOUT, TokioTcpStream::connect(addr)).await {
            Ok(Ok(stream)) => {
                println!("✅ 成功连接到设备 {}:{}", ip, port);
                
                // 生成设备标识符
                let device_id = format!("server_{}:{}", ip, port);
                
                // 保存连接
                self.connections.lock().await.insert(device_id.clone(), stream);
                
                // 启动消息接收任务
                let message_sender = self.message_sender.clone();
                let device_name = self.device_name.clone();
                let connections = self.connections.clone();
                let device_id_clone = device_id.clone();
                
                tokio::spawn(async move {
                    loop {
                        // 检查连接是否仍然存在
                        let has_connection = {
                            let conns = connections.lock().await;
                            conns.contains_key(&device_id_clone)
                        };
                        
                        if !has_connection {
                            break;
                        }
                        
                        // 尝试读取消息
                        let read_result = {
                            let mut conns = connections.lock().await;
                            if let Some(stream) = conns.get_mut(&device_id_clone) {
                                Self::read_message(stream, message_sender.clone(), device_name.clone()).await
                            } else {
                                break;
                            }
                        };
                        
                        // 如果读取失败，可能是连接断开
                        if let Err(e) = read_result {
                            eprintln!("❌ 读取消息失败: {}", e);
                            // 从连接池中移除连接
                            connections.lock().await.remove(&device_id_clone);
                            break;
                        }
                        
                        // 短暂休眠以避免忙等待
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    
                    println!("📤 断开与 {} 的连接", device_id_clone);
                });
                
                Ok(device_id)
            }
            Ok(Err(e)) => Err(anyhow::anyhow!("连接失败: {}", e)),
            Err(_) => Err(anyhow::anyhow!("连接超时")),
        }
    }

    /// 广播剪贴板消息到所有连接的设备
    pub async fn broadcast_message(&self, message: ClipboardMessage) -> Result<()> {
        let data = message.to_bytes()?;
        let message_len = data.len() as u32;
        
        // 准备发送的数据：4字节长度 + 消息内容
        let mut send_data = Vec::with_capacity(4 + data.len());
        send_data.extend_from_slice(&message_len.to_be_bytes());
        send_data.extend_from_slice(&data);
        
        // 记录日志
        match &message.content {
            ClipboardContent::Text(text) => {
                println!("📤 广播文本内容: {}", text);
            }
            ClipboardContent::Image { width, height, .. } => {
                println!("📤 广播图片内容: {}x{}", width, height);
            }
        }
        
        // 向所有连接的设备发送消息
        let mut connections = self.connections.lock().await;
        let mut failed_connections = Vec::new();
        println!("connections len: {}", connections.len());
        for (device_id, stream) in connections.iter_mut() {
            match stream.write_all(&send_data).await {
                Ok(_) => {
                    println!("✅ 消息已发送到: {}", device_id);
                }
                Err(e) => {
                    eprintln!("❌ 发送到 {} 失败: {}", device_id, e);
                    failed_connections.push(device_id.clone());
                }
            }
        }
        
        // 清理失败的连接
        for device_id in failed_connections {
            connections.remove(&device_id);
        }
        
        Ok(())
    }

    /// 广播文本内容
    pub async fn broadcast_clipboard(&self, content: &str) -> Result<()> {
        // 使用固定ID作为发送者ID
        let message = ClipboardMessage::new_text(
            content.to_string(),
            "local_device".to_string(),
            self.device_name.clone(),
        );
        self.broadcast_message(message).await
    }

    /// 广播图片内容
    pub async fn broadcast_image(&self, width: u32, height: u32, data: Vec<u8>) -> Result<()> {
        // 使用固定ID作为发送者ID
        let message = ClipboardMessage::new_image(
            width,
            height,
            data,
            "local_device".to_string(),
            self.device_name.clone(),
        );
        self.broadcast_message(message).await
    }

    /// 停止网络服务
    pub async fn shutdown(&self) {
        *self.is_running.lock().await = false;
        
        // 关闭所有连接
        self.connections.lock().await.clear();
        
        println!("🔴 网络服务已停止");
    }

    /// 获取设备名称
    pub fn get_device_name(&self) -> &str {
        &self.device_name
    }
}