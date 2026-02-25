use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::collections::HashMap;

pub struct ServicePublisher {
    name: String,
    service_type: String,
    port: u16,
    properties: HashMap<String, String>,
}

impl ServicePublisher {
    pub fn new(name: &str, service_type: &str, port: u16) -> Self {
        Self {
            name: name.to_string(),
            service_type: service_type.to_string(),
            port,
            properties: HashMap::new(),
        }
    }

    /// 添加属性
    pub fn add_property(mut self, key: String, value: String) -> Self {
        self.properties.insert(key, value);
        self
    }

    pub fn publish(&self) -> Result<String, Box<dyn std::error::Error>> {
        let daemon = ServiceDaemon::new()?;

        // 获取本地 IP 地址
        let ip_addr = get_local_ip()?;

        // 创建服务信息
        // 注意：ServiceInfo::new 的签名是：
        // new(ty_domain: &str, my_name: &str, host_name: &str,
        //     addresses: impl Into<IpAddr>, port: u16,
        //     properties: impl IntoTxtProperties)

        let my_service = ServiceInfo::new(
            &self.service_type,      // 服务类型，如 "_game._tcp.local."
            &self.name,              // 实例名称
            &get_hostname()?,        // 主机名
            ip_addr,                 // IP 地址
            self.port,               // 端口
            self.properties.clone(), // 属性（HashMap<String, String>）
        )?;

        // 注册服务
        daemon.register(my_service)?;

        let service_name = format!("{}.{}._local.", self.name, self.service_type);
        Ok(service_name)
    }
}

fn get_local_ip() -> Result<std::net::IpAddr, Box<dyn std::error::Error>> {
    // 尝试连接到一个外部地址以获取本地 IP
    let socket = std::net::UdpSocket::bind("0.0.0.0:0")?;
    socket.connect("8.8.8.8:80")?;

    let local_addr = socket.local_addr()?;
    Ok(local_addr.ip())
}

fn get_hostname() -> Result<String, Box<dyn std::error::Error>> {
    Ok(hostname::get()?
        .into_string()
        .unwrap_or_else(|_| "unknown".to_string()))
}
