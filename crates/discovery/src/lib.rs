use bevy::app::AppExit;
use bevy::prelude::*;
use bevy::time::common_conditions;
use gethostname::gethostname;
use local_ip_address::list_afinet_netifas;
use mdns_sd::{Receiver, ServiceDaemon, ServiceEvent, ServiceInfo};
use std::collections::HashMap;
use std::time::Duration;

// ///服务发现与注册
// --------------- PLUGIN --------------- //
// 服务发现插件
pub struct ServiceDiscoveryPlugin;

impl Plugin for ServiceDiscoveryPlugin {
    fn build(&self, app: &mut App) {
        // 初始化空的设备列表
        app.init_resource::<PeerList>()
            // 初始化计时器
            .init_resource::<ServiceBroadcastTimer>()
            // 仅仅添加监听系统，初始化系统由 main 控制顺序
            .add_systems(
                Update,
                (
                    listen_for_service,
                    keep_alive_broadcast,
                    // 使用 Bevy 自带的定时器控制运行频率
                    cleanup_expired_peers
                        .run_if(common_conditions::on_timer(Duration::from_secs(1))),
                ),
            )
            .add_systems(Last, shutdown_service);
    }
}

// --------------- SETUP --------------- //
// 初始化mdns守护进程
pub fn initialize_mdns_daemon(mut commands: Commands) {
    // 获取主机名字
    let hostname = get_cross_platform_hostname();
    commands.insert_resource(DeviceMetadata {
        hostname,
        os: std::env::consts::OS.to_string(),
    });

    let daemon = ServiceDaemon::new().unwrap_or_else(|e| {
        error!("Failed to create mDNS daemon: {}", e);
        // 或者直接返回
        std::process::exit(1);
    });

    // 在启动时仅调用一次 browse
    let service_type = "_portalo._tcp.local.";
    let receiver = daemon
        .browse(service_type)
        .expect("Failed to start browsing");

    commands.insert_resource(MdnsManager { daemon, receiver });

    info!("✅ mDNS Manager & Browser started");
}

// --------------- RESOURCES --------------- //
// 守护进程资源
#[derive(Resource)]
pub struct MdnsManager {
    pub daemon: ServiceDaemon,
    pub receiver: Receiver<ServiceEvent>,
}

// 我的设备信息
#[derive(Resource)]
pub struct DeviceMetadata {
    // 主机名字
    pub hostname: String,
    pub os: String,
}

// 发现的设备
#[derive(Resource, Default)]
pub struct PeerList {
    // Key 为 fullname, Value 为设备信息
    pub peers: HashMap<String, PeerInfo>,
}

// 服务广播计时器
#[derive(Resource)]
pub struct ServiceBroadcastTimer(pub Timer);

impl Default for ServiceBroadcastTimer {
    fn default() -> Self {
        // 每 30 秒广播一次
        Self(Timer::from_seconds(30.0, TimerMode::Repeating))
    }
}

// --------------- COMPONENTS --------------- //

// --------------- SYSTEMS --------------- //
// 发布服务
pub fn auto_publish_service(mdns_res: Res<MdnsManager>, device_metadata: Res<DeviceMetadata>) {
    let service_type = "_portalo._tcp.local.";
    let port = 5353;
    let hostname_suffix = get_hostname_with_suffix(&device_metadata.hostname);

    if let Ok(network_interfaces) = list_afinet_netifas() {
        for (name, ip) in network_interfaces {
            // 过滤：非回环且IPv4接口
            if !ip.is_loopback() && ip.is_ipv4() {
                // 为每个网卡创建一个唯一的实例名称（Dukto 识别需要）
                let instance_name = format!("portalo-{}-{}", &device_metadata.hostname, name);
                // 使用当前系统时间作为心跳版本
                let hb_version = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
                    .to_string();
                // 额外信息, hb_version变动的值会触发其他节点的 Resolved 事件
                let properties = [
                    ("os", device_metadata.os.clone()),
                    ("ver", "0.1.0".to_string()),
                    ("hb", hb_version),
                ];

                let my_service = ServiceInfo::new(
                    service_type,
                    &instance_name,
                    &hostname_suffix,
                    ip,
                    port,
                    &properties[..],
                )
                .unwrap();

                // 注册
                if let Err(e) = mdns_res.daemon.register(my_service) {
                    error!("❌ Failed to register on {}: {}", name, e);
                } else {
                    info!("✨ Published Portalo on {} ({})", name, ip);
                }
            }
        }
    }
}

// 监听服务
fn listen_for_service(mdns: Res<MdnsManager>, mut peer_list: ResMut<PeerList>, time: Res<Time>) {
    while let Ok(event) = mdns.receiver.try_recv() {
        match event {
            // 解析
            ServiceEvent::ServiceResolved(info) => {
                info!("❌ Service resolved: {} ", info.fullname);
                let hostname = info.get_hostname().to_string();
                // 过滤出所有 IPv4 地址并转为字符串
                let ips: Vec<String> = info
                    .get_addresses()
                    .iter()
                    .filter(|ip| ip.is_ipv4())
                    .map(|ip| ip.to_string())
                    .collect();

                // 如果没搜到有效 IP 则跳过
                if ips.is_empty() {
                    continue;
                }

                // 提取 OS 信息（Dukto 风格图标显示的关键）
                let os = info
                    .get_properties()
                    .iter()
                    .find(|p| p.key() == "os")
                    .map(|p| p.val_str().to_string())
                    .unwrap_or_else(|| "unknown".to_string());

                // 设备名
                let device_name = extract_device_name(&info.fullname);

                // 更新或创建 Peer 记录
                let entry = peer_list
                    .peers
                    .entry(device_name.clone())
                    .or_insert(PeerInfo {
                        name: hostname.clone(),
                        ips: vec![],
                        os,
                        last_seen: time.elapsed_secs_f64(),
                    });

                entry.last_seen = time.elapsed_secs_f64();
                for ip in ips.iter() {
                    if !entry.ips.contains(ip) {
                        entry.ips.push(ip.to_string());
                    }
                }
            }
            // 断开
            ServiceEvent::ServiceRemoved(service_type, fullname) => {
                info!("❌ Service removed: {} (type: {})", fullname, service_type);

                // 设备名
                let device_name = extract_device_name(&fullname);
                peer_list.peers.remove(&device_name);
                info!("🗑️  Removed peer from list: {}", device_name);
            }
            // 发现
            ServiceEvent::ServiceFound(service_type, fullname) => {
                debug!("🔍 Service found: {} (type: {})", fullname, service_type);
            }
            // 启动
            ServiceEvent::SearchStarted(service_type) => {
                info!("▶️  Started searching for: {}", service_type);
            }
            // 停止
            ServiceEvent::SearchStopped(service_type) => {
                info!("⏹️  Stopped searching for: {}", service_type);
            }
            // 其它
            _ => {
                trace!("Received other ServiceEvent");
            }
        }
    }
}

// 关闭服务
pub fn shutdown_service(
    exit: MessageReader<AppExit>,
    mdns: Res<MdnsManager>,
    device_metadata: Res<DeviceMetadata>,
) {
    if !exit.is_empty() {
        let service_type = "_portalo._tcp.local.";
        // 必须与注册时的 instance_name 完全一致
        if let Ok(network_interfaces) = list_afinet_netifas() {
            for (name, ip) in network_interfaces {
                // 过滤：非回环且IPv4接口
                if !ip.is_loopback() && ip.is_ipv4() {
                    // 为每个网卡创建一个唯一的实例名称（Dukto 识别需要）
                    let instance_name =
                        format!("portalo-{}-{}", device_metadata.hostname.clone(), name);

                    let full_name = format!("{}.{}", instance_name, service_type);
                    // 注销
                    if let Err(e) = mdns.daemon.unregister(&full_name) {
                        error!("❌ 无法注销服务: {}", e);
                    } else {
                        info!("👋 服务已主动下线: {}", full_name);
                    }
                }
            }
        }
    }
}

// 定期广播
pub fn keep_alive_broadcast(
    time: Res<Time>,
    mut timer: ResMut<ServiceBroadcastTimer>,
    mdns_res: Res<MdnsManager>,
    device_metadata: Res<DeviceMetadata>,
) {
    if timer.0.tick(time.delta()).just_finished() {
        info!("📡 Sending keep-alive broadcast...");

        // 重新调用你之前定义的发布逻辑
        // 注意：mdns-sd 的 register 如果实例名相同，通常会覆盖旧的记录
        auto_publish_service(mdns_res, device_metadata);
    }
}

// 清理过期的peer
fn cleanup_expired_peers(mut peer_list: ResMut<PeerList>, time: Res<Time>) {
    let now = time.elapsed_secs_f64();
    // 45秒超时(心跳30秒)
    let timeout = 45.0;

    // retain 会保留返回 true 的项，移除返回 false 的项
    peer_list.peers.retain(|name, info| {
        // 自己或有效期内
        let is_alive = (now - info.last_seen) < timeout;

        if !is_alive {
            info!("⏳ Peer timed out: {}", name);
        }
        is_alive
    });
}

// --------------- OTHER --------------- //
// 设备信息
#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub name: String,
    pub ips: Vec<String>,
    pub os: String,
    pub last_seen: f64,
}

// 获取主机名字 eg: devuan
fn get_cross_platform_hostname() -> String {
    let raw_name = gethostname()
        .into_string()
        .unwrap_or_else(|_| "PortaloDevice".into());

    // 移动端处理：转义空格和特殊字符，确保 mDNS 兼容
    raw_name
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
}

// 获取带后缀的主机名(用于mdns) eg: devuan.local.
fn get_hostname_with_suffix(hostname: &str) -> String {
    format!("{}.local.", hostname.trim_end_matches('.'))
}

// 从fullname中提取设备名
fn extract_device_name(fullname: &str) -> String {
    // 1. 获取第一个点之前的部分 (Instance Name)
    // "portalo-MyLaptop-eth0._portalo._tcp.local." -> "portalo-MyLaptop-eth0"
    let instance_name = fullname.split('.').next().unwrap_or(fullname);

    // 2. 去掉自定义协议前缀 (如有)
    // "portalo-MyLaptop-eth0" -> "MyLaptop-eth0"
    let raw_name = instance_name
        .strip_prefix("portalo-")
        .unwrap_or(instance_name);

    // 3. 处理网卡后缀
    // mDNS 注册时为了唯一性，通常会在末尾加上网卡名 (如 -eth0, -wlan0)
    // 我们寻找最后一个连字符并切分
    if let Some((name, _interface)) = raw_name.rsplit_once('-') {
        name.to_string()
    } else {
        // 如果没有连字符，说明 instance_name 就是设备名本身
        raw_name.to_string()
    }
}
