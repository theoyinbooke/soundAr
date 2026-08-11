"""soundAr Design System — Light / Cream theme.

Warm off-white backgrounds, clear text hierarchy, blue accent.
"""

COLORS = {
    # Backgrounds (light cream palette)
    "bg_base": "#f3f1ec",        # Sidebar — warm cream
    "bg_primary": "#faf9f6",     # Main content area — off-white
    "bg_raised": "#ffffff",      # Cards, rows, elevated surfaces
    "bg_input": "#f0efe9",       # Input fields, dropdowns
    "bg_hover": "#eae8e3",       # Hover state on rows
    "bg_active": "#e2e0db",      # Active/pressed state

    # Borders
    "border_subtle": "#e8e6e1",  # Faint separation lines
    "border_default": "#d9d7d2", # Default borders
    "border_strong": "#c4c2bd",  # Focused inputs

    # Text hierarchy
    "text_primary": "#1a1a1a",   # Headings, model names
    "text_secondary": "#4b5563", # Body text, labels
    "text_tertiary": "#6b7280",  # Metadata, muted
    "text_ghost": "#9ca3af",     # Placeholders, timestamps
    "text_faint": "#c4c2bd",     # Disabled, de-emphasized

    # Accent — Blue
    "accent": "#3b82f6",
    "accent_hover": "#2563eb",
    "accent_pressed": "#1d4ed8",
    "accent_muted": "#eff6ff",   # Light blue tint
    "accent_text": "#2563eb",    # Blue text on light bg

    # Semantic
    "success": "#16a34a",
    "success_muted": "#f0fdf4",
    "success_text": "#15803d",
    "warning": "#d97706",
    "warning_muted": "#fffbeb",
    "warning_text": "#b45309",
    "error": "#dc2626",
    "error_muted": "#fef2f2",
    "error_text": "#b91c1c",

    # Task badge colors
    "stt_badge_bg": "#eff6ff",
    "stt_badge_text": "#2563eb",
    "tts_badge_bg": "#fffbeb",
    "tts_badge_text": "#b45309",

    # Sidebar specific
    "sidebar_bg": "#f3f1ec",
    "sidebar_active_bg": "#e8e6e1",
    "sidebar_text": "#6b7280",
    "sidebar_active_text": "#2563eb",
}

FONT_FAMILY = '-apple-system, "Inter", "SF Pro Display", "Segoe UI", sans-serif'
FONT_MONO = '"JetBrains Mono", "SF Mono", "Fira Code", "Cascadia Code", monospace'


def get_theme_stylesheet() -> str:
    return """

    /* === Global === */
    QMainWindow, QWidget {
        background-color: #faf9f6;
        color: #1a1a1a;
        font-family: -apple-system, "Inter", "SF Pro Display", "Segoe UI", sans-serif;
        font-size: 14px;
    }

    /* === Sidebar === */
    QWidget#sidebar {
        background-color: #f3f1ec;
        border-right: 1px solid #e8e6e1;
    }

    /* === Labels === */
    QLabel {
        color: #4b5563;
        background: transparent;
    }
    QLabel#title {
        font-size: 20px;
        font-weight: 500;
        color: #1a1a1a;
    }
    QLabel#subtitle {
        font-size: 13px;
        color: #9ca3af;
    }
    QLabel#sectionTitle {
        font-size: 16px;
        font-weight: 500;
        color: #1a1a1a;
    }
    QLabel#metadata {
        font-size: 12px;
        color: #9ca3af;
    }
    QLabel#modelName {
        font-size: 14px;
        font-weight: 500;
        color: #1a1a1a;
    }
    QLabel#faint {
        font-size: 12px;
        color: #c4c2bd;
    }
    QLabel#accentLabel {
        font-size: 12px;
        color: #2563eb;
    }
    QLabel#successLabel {
        font-size: 12px;
        color: #16a34a;
    }
    QLabel#errorLabel {
        font-size: 12px;
        color: #dc2626;
    }
    QLabel#monoLabel {
        font-family: "JetBrains Mono", "SF Mono", "Fira Code", "Cascadia Code", monospace;
        font-size: 12px;
        color: #6b7280;
    }

    /* === Buttons === */
    QPushButton {
        background-color: transparent;
        color: #4b5563;
        border: 1px solid #d9d7d2;
        border-radius: 6px;
        padding: 6px 16px;
        font-size: 12px;
        font-weight: 400;
    }
    QPushButton:hover {
        background-color: #f0efe9;
        border-color: #c4c2bd;
    }
    QPushButton:pressed {
        background-color: #e2e0db;
    }
    QPushButton:disabled {
        color: #c4c2bd;
        background-color: #f0efe9;
    }
    QPushButton#primary {
        background-color: #3b82f6;
        color: #ffffff;
        border: none;
        font-weight: 500;
    }
    QPushButton#primary:hover {
        background-color: #2563eb;
    }
    QPushButton#primary:pressed {
        background-color: #1d4ed8;
    }
    QPushButton#danger {
        color: #dc2626;
        border: 1px solid #d9d7d2;
    }
    QPushButton#danger:hover {
        border-color: #dc2626;
        background-color: #fef2f2;
    }
    QPushButton#large {
        background-color: #3b82f6;
        color: #ffffff;
        border: none;
        font-size: 14px;
        font-weight: 500;
        padding: 10px 28px;
        border-radius: 8px;
    }
    QPushButton#large:hover {
        background-color: #2563eb;
    }
    QPushButton#large:disabled {
        background-color: #d9d7d2;
        color: #9ca3af;
    }

    /* Toggle buttons (e.g. STT/TTS tabs) */
    QPushButton#toggle {
        background-color: transparent;
        color: #6b7280;
        border: 1px solid #d9d7d2;
        border-radius: 6px;
        padding: 6px 20px;
        font-size: 13px;
        font-weight: 500;
    }
    QPushButton#toggle:hover {
        background-color: #f0efe9;
    }
    QPushButton#toggle:checked {
        background-color: #3b82f6;
        color: #ffffff;
        border-color: #3b82f6;
    }
    QPushButton#toggle:checked:hover {
        background-color: #2563eb;
    }

    /* === Line Edits === */
    QLineEdit {
        background-color: #ffffff;
        color: #1a1a1a;
        border: 1px solid #d9d7d2;
        border-radius: 8px;
        padding: 8px 14px;
        font-size: 13px;
        selection-background-color: #bfdbfe;
    }
    QLineEdit:focus {
        border-color: #3b82f6;
    }

    /* === Text Edits === */
    QTextEdit, QPlainTextEdit {
        background-color: #ffffff;
        color: #1a1a1a;
        border: 1px solid #d9d7d2;
        border-radius: 12px;
        padding: 16px 20px;
        font-size: 14px;
        selection-background-color: #bfdbfe;
    }
    QTextEdit:focus, QPlainTextEdit:focus {
        border-color: #3b82f6;
    }

    /* === Combo Boxes === */
    QComboBox {
        background-color: #ffffff;
        color: #4b5563;
        border: 1px solid #d9d7d2;
        border-radius: 8px;
        padding: 0px 14px;
        font-size: 13px;
    }
    QComboBox:hover {
        border-color: #c4c2bd;
    }
    QComboBox::drop-down {
        border: none;
        width: 0px;
    }
    QComboBox::down-arrow {
        image: none;
        width: 0px;
        height: 0px;
    }
    QComboBox QAbstractItemView {
        background-color: #ffffff;
        color: #4b5563;
        border: 1px solid #d9d7d2;
        border-radius: 8px;
        padding: 4px;
        selection-background-color: #eff6ff;
        selection-color: #1a1a1a;
    }

    /* === Progress Bars === */
    QProgressBar {
        background-color: #e2e0db;
        border: none;
        border-radius: 2px;
        max-height: 4px;
        min-height: 4px;
        text-align: center;
    }
    QProgressBar::chunk {
        background-color: #3b82f6;
        border-radius: 2px;
    }

    /* === Scroll Bars === */
    QScrollBar:vertical {
        background: transparent;
        width: 6px;
        margin: 0;
    }
    QScrollBar::handle:vertical {
        background: #d9d7d2;
        border-radius: 3px;
        min-height: 24px;
    }
    QScrollBar::handle:vertical:hover {
        background: #c4c2bd;
    }
    QScrollBar::add-line:vertical, QScrollBar::sub-line:vertical,
    QScrollBar::add-page:vertical, QScrollBar::sub-page:vertical {
        background: transparent;
        height: 0;
    }
    QScrollBar:horizontal {
        background: transparent;
        height: 6px;
    }
    QScrollBar::handle:horizontal {
        background: #d9d7d2;
        border-radius: 3px;
        min-width: 24px;
    }

    /* === Sliders === */
    QSlider::groove:horizontal {
        background: #e2e0db;
        height: 3px;
        border-radius: 1px;
    }
    QSlider::handle:horizontal {
        background: #4b5563;
        width: 12px;
        height: 12px;
        border-radius: 6px;
        margin: -5px 0;
    }
    QSlider::sub-page:horizontal {
        background: #3b82f6;
        border-radius: 1px;
    }

    /* === Check Boxes === */
    QCheckBox {
        color: #4b5563;
        font-size: 13px;
        spacing: 8px;
    }
    QCheckBox::indicator {
        width: 16px;
        height: 16px;
        border-radius: 4px;
        border: 1px solid #d9d7d2;
        background: #ffffff;
    }
    QCheckBox::indicator:checked {
        background: #3b82f6;
        border-color: #3b82f6;
    }

    /* === Group Boxes === */
    QGroupBox {
        color: #1a1a1a;
        font-size: 14px;
        font-weight: 500;
        border: 1px solid #e8e6e1;
        border-radius: 12px;
        margin-top: 16px;
        padding-top: 24px;
    }
    QGroupBox::title {
        subcontrol-origin: margin;
        left: 20px;
        padding: 0 8px;
    }

    /* === Splitters === */
    QSplitter::handle {
        background: #e8e6e1;
        width: 1px;
    }

    /* === Tool Tips === */
    QToolTip {
        background-color: #1a1a1a;
        color: #ffffff;
        border: none;
        border-radius: 6px;
        padding: 6px 10px;
        font-size: 12px;
    }

    /* === Status Bar === */
    QStatusBar {
        background-color: #f3f1ec;
        color: #9ca3af;
        font-size: 11px;
        border-top: 1px solid #e8e6e1;
    }

    /* === Tables === */
    QTableWidget {
        background-color: #ffffff;
        color: #4b5563;
        border: 1px solid #e8e6e1;
        border-radius: 12px;
        gridline-color: #e8e6e1;
        font-size: 13px;
    }
    QTableWidget::item {
        padding: 8px 16px;
        border-bottom: 1px solid #e8e6e1;
    }
    QTableWidget::item:selected {
        background-color: #eff6ff;
        color: #1a1a1a;
    }
    QHeaderView::section {
        background-color: #faf9f6;
        color: #9ca3af;
        font-size: 11px;
        font-weight: 500;
        padding: 8px 16px;
        border: none;
        border-bottom: 1px solid #e8e6e1;
    }

    /* === Frames === */
    QFrame#card {
        background-color: #ffffff;
        border: 1px solid #e8e6e1;
        border-radius: 12px;
    }
    QFrame#modelRow {
        background-color: #ffffff;
        border: none;
    }

    /* === Scroll Area === */
    QScrollArea {
        background: transparent;
        border: none;
    }
    QScrollArea > QWidget > QWidget {
        background: transparent;
    }

    /* === Menus === */
    QMenu {
        background-color: #ffffff;
        color: #4b5563;
        border: 1px solid #d9d7d2;
        border-radius: 8px;
        padding: 4px;
    }
    QMenu::item {
        padding: 6px 16px;
        border-radius: 4px;
    }
    QMenu::item:selected {
        background-color: #eff6ff;
        color: #1a1a1a;
    }
    QMenu::separator {
        height: 1px;
        background: #e8e6e1;
        margin: 4px 8px;
    }

    /* === Dialog === */
    QDialog {
        background-color: #faf9f6;
        color: #1a1a1a;
    }
    QMessageBox {
        background-color: #faf9f6;
        color: #1a1a1a;
    }
    QMessageBox QWidget {
        background-color: #faf9f6;
        color: #1a1a1a;
    }
    QMessageBox QLabel {
        background: transparent;
        color: #4b5563;
        font-size: 13px;
    }
    QMessageBox QPushButton {
        background-color: #ffffff;
        color: #4b5563;
        border: 1px solid #d9d7d2;
        border-radius: 6px;
        padding: 6px 16px;
        min-width: 88px;
    }
    QMessageBox QPushButton:hover {
        background-color: #f0efe9;
        border-color: #c4c2bd;
    }
    QMessageBox QPushButton:pressed {
        background-color: #e2e0db;
    }
    QDialogButtonBox QPushButton {
        min-width: 80px;
    }
    """
