pub mod tool_home;
pub mod tool_detail;
pub mod tcp_udp_test;
pub mod ip_scanner;
pub mod ip_calculator;

use crate::custom_icons::CustomIconName;
use gpui_component::IconName;

/// 应用内建工具定义
#[derive(Clone)]
pub struct Tool {
    /// 工具唯一标识
    pub id: &'static str,
    /// 工具显示名称
    pub name: &'static str,
    /// 工具图标
    pub icon: CustomIconName,
    /// 工具简短描述
    pub description: &'static str,
}

impl Tool {
    /// 根据工具 ID 查找工具
    pub fn from_id(id: &str) -> Option<&'static Tool> {
        AVAILABLE_TOOLS.iter().find(|t| t.id == id)
    }
}

/// 所有可用的内置工具列表
pub const AVAILABLE_TOOLS: &[Tool] = &[
    Tool {
        id: "tcp_udp_test",
        name: "网络调试工具",
        icon: CustomIconName::IconName(IconName::SquareTerminal),
        description: "支持 TCP/UDP 客户端和服务端模式，快速验证网络通信逻辑和数据格式。",
    },
    Tool {
        id: "ip_scanner",
        name: "IP 地址探测",
        icon: CustomIconName::IconName(IconName::Network),
        description: "支持IP网段、连续IP地址、单个IP地址扫描检查其IP是否正在使用。",
    },
    Tool {
        id: "ip_calculator",
        name: "IP 计算器",
        icon: CustomIconName::Calculator,
        description: "支持输入IP/mask 或 IP/subnet来计算其网络信息。",
    },
];

/// 当前视图枚举
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CurrentView {
    /// 工具卡片首页
    ToolHome,
    /// 工具详情页面
    ToolDetail { tool_id: String },
}

impl CurrentView {
    pub fn is_tool_home(&self) -> bool {
        matches!(self, CurrentView::ToolHome)
    }
}
