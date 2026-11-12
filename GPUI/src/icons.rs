//! Material Symbols (Outlined, Apache-2.0) embedded in the binary and served to gpui's `svg()`.

use std::borrow::Cow;

use gpui::{AssetSource, Hsla, Result, SharedString, Svg, prelude::*, px, svg};

use crate::model::{Category, Role};

macro_rules! icons {
    ($($variant:ident => $file:literal),* $(,)?) => {
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum Icon { $($variant),* }

        impl Icon {
            const ALL: &[(&str, &[u8])] = &[$((concat!("icons/", $file, ".svg"), include_bytes!(concat!("../assets/icons/", $file, ".svg")))),*];

            pub fn path(self) -> &'static str {
                match self { $(Icon::$variant => concat!("icons/", $file, ".svg")),* }
            }
        }
    };
}

icons! {
    Add => "add_circle",
    AddDraft => "add",
    Archive => "archive",
    Attachment => "attach_file",
    Back => "chevron_left",
    Calendar => "calendar_month_fill",
    Chat => "forum",
    Check => "check",
    Close => "close",
    Compose => "edit_square",
    Delete => "delete",
    Disclosure => "chevron_right",
    Draft => "draft",
    Dock => "right_panel_open",
    Float => "picture_in_picture",
    Folder => "folder",
    Forward => "forward",
    Inbox => "inbox",
    Label => "label",
    Location => "location_on",
    Newsletter => "newspaper_fill",
    Notification => "notifications_fill",
    People => "group_fill",
    OpenInWindow => "open_in_new",
    Minimize => "minimize",
    MarkRead => "mark_email_read",
    MarkUnread => "mark_email_unread",
    Person => "person",
    Refresh => "refresh",
    Reply => "reply",
    ReplyAll => "reply_all",
    Resolve => "check_circle",
    Restore => "move_to_inbox",
    Search => "search",
    Sent => "send",
    Sidebar => "left_panel_close",
    Spam => "report",
    Swap => "swap_horiz",
    Important => "priority_high",
    Star => "star",
    Starred => "star_fill",
    Today => "today",
    Video => "videocam",
    VideoFill => "videocam_fill",
    Waiting => "schedule",
}

impl Icon {
    pub fn for_role(role: Role) -> Icon {
        match role {
            Role::Inbox => Icon::Inbox,
            Role::Starred => Icon::Star,
            Role::Important => Icon::Important,
            Role::Sent => Icon::Sent,
            Role::Drafts => Icon::Draft,
            Role::Archive | Role::All => Icon::Archive,
            Role::Junk => Icon::Spam,
            Role::Trash => Icon::Delete,
            Role::Other => Icon::Folder,
        }
    }

    /// A Smart Inbox bundle's glyph.
    pub fn for_category(category: Category) -> Icon {
        match category {
            Category::People => Icon::People,
            Category::Notification => Icon::Notification,
            Category::Newsletter => Icon::Newsletter,
            Category::Invitation => Icon::Calendar,
            Category::Other => Icon::Folder,
        }
    }
}

pub fn icon(icon: Icon, size: f32, color: Hsla) -> Svg {
    svg().path(icon.path()).flex_shrink_0().size(px(size)).text_color(color)
}

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(Icon::ALL.iter().find(|(name, _)| *name == path).map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(Icon::ALL.iter().filter(|(name, _)| name.starts_with(path)).map(|(name, _)| (*name).into()).collect())
    }
}
