#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolKind {
    Shell,
    FileRead,
    FileEdit,
    FileWrite,
    Search,
    Web,
    Other,
}

impl ToolKind {
    pub fn emoji(self) -> &'static str {
        match self {
            ToolKind::Shell => "🔧",
            ToolKind::FileRead => "📖",
            ToolKind::FileEdit => "✏️",
            ToolKind::FileWrite => "📝",
            ToolKind::Search => "🔍",
            ToolKind::Web => "🌐",
            ToolKind::Other => "⚙️",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ToolKind::Shell => "Shell",
            ToolKind::FileRead => "FileRead",
            ToolKind::FileEdit => "FileEdit",
            ToolKind::FileWrite => "FileWrite",
            ToolKind::Search => "Search",
            ToolKind::Web => "Web",
            ToolKind::Other => "Other",
        }
    }
}
