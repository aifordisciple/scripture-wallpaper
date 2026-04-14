import { useState, useEffect, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { convertFileSrc } from "@tauri-apps/api/core";
import { enable, disable, isEnabled } from "@tauri-apps/plugin-autostart";
import "./styles.css";

interface AppConfig {
  font_size: number;
  update_time: string;
  font_path: string;
  wallpaper_mode: "bing" | "picsum" | "local";
  local_folder: string;
  img_api_url: string;
}

type TabId = "dashboard" | "favorites" | "settings" | "logs";

interface NavItem {
  id: TabId;
  label: string;
  icon: string;
}

const NAV_ITEMS: NavItem[] = [
  { id: "dashboard", label: "主控制台", icon: "\u2302" },
  { id: "favorites", label: "我的收藏", icon: "\u2606" },
  { id: "settings", label: "偏好设置", icon: "\u2699" },
  { id: "logs", label: "系统日志", icon: "\u2261" },
];

interface FavoriteItem {
  id: number;
  content: string;
  reference: string;
  imagePath: string;
  createdAt: string;
}

function getLogClassName(log: string): string {
  if (log.includes("[错误]") || log.includes("[初始化错误]")) return "log-entry-error";
  if (log.includes("[完成]")) return "log-entry-success";
  if (log.includes("[收藏]")) return "log-entry-warning";
  if (log.includes("[系统]") || log.includes("[设置]")) return "log-entry-system";
  return "log-entry-info";
}

function App() {
  const [activeTab, setActiveTab] = useState<TabId>("dashboard");
  const [logs, setLogs] = useState<string[]>([]);
  const [isProcessing, setIsProcessing] = useState(false);
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [autoStart, setAutoStart] = useState(false);
  const [previewUrl, setPreviewUrl] = useState<string | null>(null);
  const [currentScripture, setCurrentScripture] = useState<string | null>(null);
  const [currentWallpaperPath, setCurrentWallpaperPath] = useState<string | null>(null);
  const [favorites, setFavorites] = useState<FavoriteItem[]>([]);
  const logsEndRef = useRef<HTMLDivElement>(null);

  const addLog = (msg: string) => setLogs((prev) => [...prev, msg]);

  const loadFavorites = async () => {
    try {
      const rows = await invoke<[number, string, string, string, string][]>("list_favorites", {
        dbPath: "scriptures.db",
      });
      const items = rows.map(([id, content, reference, imagePath, createdAt]) => ({
        id,
        content,
        reference,
        imagePath,
        createdAt,
      }));
      setFavorites(items);
    } catch {
      // Favorites table may not exist yet
    }
  };

  // Auto-scroll logs
  useEffect(() => {
    logsEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [logs]);

  // Initialize app
  useEffect(() => {
    const initApp = async () => {
      try {
        const loadedConfig: AppConfig = await invoke("load_config", { configPath: "app_config.json" });
        setConfig(loadedConfig);
        addLog("[系统] 本地参数配置加载成功");

        const dbRes = await invoke("init_database", { dbPath: "scriptures.db" });
        addLog(`[系统] ${dbRes}`);

        await loadFavorites();

        try {
          const autostartStatus = await isEnabled();
          setAutoStart(autostartStatus);
        } catch {
          // Autostart plugin may not be available on all platforms
        }
      } catch (err) {
        addLog(`[初始化错误] ${err}`);
      }
    };
    initApp();

    const unlistenUpdate = listen("trigger-update", () => executeDailyUpdate());

    const currentWindow = getCurrentWindow();
    const unlistenClose = currentWindow.onCloseRequested(async (event) => {
      event.preventDefault();
      await currentWindow.hide();
    });

    return () => {
      unlistenUpdate.then((f) => f());
      unlistenClose.then((f) => f());
    };
  }, []);

  // Save config
  const handleSaveConfig = async () => {
    if (!config) return;
    try {
      const res = await invoke("save_config", { configPath: "app_config.json", config });
      addLog(`[设置] ${res}`);
    } catch (err) {
      addLog(`[设置错误] ${err}`);
    }
  };

  // Core business flow
  const executeDailyUpdate = useCallback(async () => {
    if (isProcessing || !config) return;
    setIsProcessing(true);
    addLog("--- 开始多源壁纸更新流程 ---");

    try {
      const [content, reference] = await invoke<[string, string]>("get_random_scripture", {
        dbPath: "scriptures.db",
      });
      const fullText = `${content} —— ${reference}`;
      setCurrentScripture(fullText);
      addLog(`获取经文: ${fullText}`);

      let inputImagePath = "temp_bg.jpg";
      const timestamp = Date.now();

      if (config.wallpaper_mode === "bing") {
        addLog(">> 正在拉取 Bing 壁纸...");
        await invoke("fetch_bing_daily", { savePath: inputImagePath });
      } else if (config.wallpaper_mode === "local") {
        addLog(`>> 正在扫描本地图库: ${config.local_folder}`);
        inputImagePath = await invoke<string>("get_random_local_image", {
          folderPath: config.local_folder,
        });
        addLog(`>> 抽中本地图片: ${inputImagePath}`);
      } else {
        const url = config.img_api_url.includes("?")
          ? `${config.img_api_url}&random=${timestamp}`
          : `${config.img_api_url}?random=${timestamp}`;
        addLog(">> 正在拉取 Picsum 随机风景...");
        await invoke("download_image", { url, savePath: inputImagePath });
      }

      const outputFilename = `wallpaper_${timestamp}.jpg`;
      addLog(">> 正在进行智能排版合成...");
      const outputPath = await invoke<string>("generate_wallpaper", {
        inputPath: inputImagePath,
        outputPath: outputFilename,
        text: fullText,
        fontPath: config.font_path,
        fontSize: config.font_size,
      });

      setCurrentWallpaperPath(outputPath);

      await invoke("set_system_wallpaper", { wallpaperPath: outputFilename });
      addLog("[完成] 桌面已更新为今日启示。");

      setPreviewUrl(convertFileSrc(outputPath));
    } catch (error) {
      addLog(`[错误] 流程中断: ${error}`);
    } finally {
      setIsProcessing(false);
    }
  }, [isProcessing, config]);

  // Favorite wallpaper
  const handleFavorite = async () => {
    if (!currentScripture || !currentWallpaperPath) {
      addLog("[收藏] 请先刷新壁纸后再收藏");
      return;
    }
    try {
      const parts = currentScripture.split(" —— ");
      await invoke("add_favorite", {
        dbPath: "scriptures.db",
        content: parts[0] || currentScripture,
        reference: parts[1] || "",
        imagePath: currentWallpaperPath,
      });
      addLog("[收藏] 壁纸已收藏");
      await loadFavorites();
    } catch (err) {
      addLog(`[收藏错误] ${err}`);
    }
  };

  // Apply a favorite wallpaper to desktop
  const handleApplyFavorite = async (imagePath: string) => {
    try {
      await invoke("set_system_wallpaper", { wallpaperPath: imagePath });
      addLog("[收藏] 已将收藏壁纸设为桌面");
    } catch (err) {
      addLog(`[收藏错误] 设置桌面失败: ${err}`);
    }
  };

  // Remove a favorite
  const handleRemoveFavorite = async (id: number) => {
    try {
      await invoke("remove_favorite", { dbPath: "scriptures.db", favoriteId: id });
      addLog("[收藏] 已移除收藏");
      await loadFavorites();
    } catch (err) {
      addLog(`[收藏错误] ${err}`);
    }
  };

  if (!config) {
    return (
      <div className="loading-screen">
        <div className="loading-spinner" />
      </div>
    );
  }

  const modeLabel = config.wallpaper_mode === "bing"
    ? "Bing 每日壁纸"
    : config.wallpaper_mode === "picsum"
      ? "Picsum 随机图库"
      : "本地自定义图库";

  return (
    <div className="app-layout">
      {/* Sidebar */}
      <aside className="sidebar">
        <div className="sidebar-brand">
          <div className="sidebar-brand-title">经文壁纸</div>
          <div className="sidebar-brand-subtitle">Scripture Wallpaper</div>
        </div>

        <nav className="sidebar-nav">
          {NAV_ITEMS.map((item) => (
            <div
              key={item.id}
              className={`sidebar-nav-item ${activeTab === item.id ? "active" : ""}`}
              onClick={() => setActiveTab(item.id)}
            >
              <span className="sidebar-nav-icon">{item.icon}</span>
              <span>{item.label}</span>
            </div>
          ))}
        </nav>

        <div className="sidebar-footer">
          <div className="sidebar-shortcut">
            快捷键 <kbd>Cmd</kbd>+<kbd>Shift</kbd>+<kbd>W</kbd>
          </div>
        </div>
      </aside>

      {/* Main Content */}
      <main className="main-content">
        {/* Dashboard */}
        {activeTab === "dashboard" && (
          <div className="panel" key="dashboard">
            <div className="dashboard-hero">
              <div className="dashboard-hero-title">今日经文</div>
              <div className="dashboard-hero-subtitle">
                应用已在后台静默运行，每日 <strong>{config.update_time}</strong> 自动更新桌面壁纸。
              </div>
            </div>

            <div className="dashboard-actions">
              <button
                className="btn btn-primary"
                onClick={executeDailyUpdate}
                disabled={isProcessing}
              >
                {isProcessing ? "生成中..." : "立即刷新壁纸"}
              </button>
              <button className="btn btn-secondary" onClick={handleFavorite}>
                <span className="btn-icon">&#9733;</span>
                收藏壁纸
              </button>
              <button
                className="btn btn-ghost"
                onClick={() => getCurrentWindow().hide()}
              >
                隐藏至托盘
              </button>
            </div>

            {isProcessing && (
              <div style={{ marginTop: "16px" }}>
                <span className="status-badge status-badge-processing">
                  <span className="status-dot" />
                  正在生成壁纸...
                </span>
              </div>
            )}

            {previewUrl && (
              <div className="preview-card">
                <img
                  className="preview-card-image"
                  src={previewUrl}
                  alt="壁纸预览"
                />
                <div className="preview-card-footer">
                  <div className="preview-label">当前经文</div>
                  <div className="preview-scripture">{currentScripture}</div>
                </div>
              </div>
            )}

            {!previewUrl && !isProcessing && (
              <div style={{ marginTop: "32px" }}>
                <span className="mode-badge">{modeLabel}</span>
              </div>
            )}
          </div>
        )}

        {/* Favorites */}
        {activeTab === "favorites" && (
          <div className="panel" key="favorites">
            <div className="favorites-title">我的收藏</div>
            <div className="favorites-subtitle">浏览和管理您收藏的壁纸，点击可设为桌面。</div>

            {favorites.length === 0 ? (
              <div className="favorites-empty">
                <div className="favorites-empty-icon">&#9734;</div>
                <div className="favorites-empty-text">暂无收藏</div>
                <div className="favorites-empty-hint">在主控制台生成壁纸后，点击「收藏壁纸」按钮即可收藏</div>
              </div>
            ) : (
              <div className="favorites-grid">
                {favorites.map((fav) => (
                  <div key={fav.id} className="favorite-card">
                    <div className="favorite-card-image-wrapper">
                      <img
                        className="favorite-card-image"
                        src={convertFileSrc(fav.imagePath)}
                        alt={fav.content}
                        onClick={() => handleApplyFavorite(fav.imagePath)}
                      />
                      <button
                        className="favorite-card-apply"
                        onClick={() => handleApplyFavorite(fav.imagePath)}
                        title="设为桌面"
                      >
                        设为桌面
                      </button>
                    </div>
                    <div className="favorite-card-info">
                      <div className="favorite-card-scripture">{fav.content}</div>
                      <div className="favorite-card-reference">{fav.reference}</div>
                      <div className="favorite-card-meta">
                        <span className="favorite-card-date">{fav.createdAt}</span>
                        <button
                          className="favorite-card-delete"
                          onClick={() => handleRemoveFavorite(fav.id)}
                          title="删除收藏"
                        >
                          删除
                        </button>
                      </div>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>
        )}

        {/* Settings */}
        {activeTab === "settings" && (
          <div className="panel" key="settings">
            <div className="settings-title">偏好设置</div>
            <div className="settings-subtitle">自定义壁纸数据源、排版样式和系统行为。</div>

            <div className="settings-group">
              {/* Wallpaper Source */}
              <div className="settings-card">
                <div className="settings-card-row">
                  <div className="settings-label">壁纸数据源</div>
                  <div className="settings-description">选择桌面壁纸的图片来源</div>
                  <select
                    className="settings-select"
                    value={config.wallpaper_mode}
                    onChange={(e) =>
                      setConfig({ ...config, wallpaper_mode: e.target.value as AppConfig["wallpaper_mode"] })
                    }
                  >
                    <option value="bing">Bing 每日精美壁纸（推荐）</option>
                    <option value="picsum">在线随机图库（Picsum）</option>
                    <option value="local">本地自定义图库（离线模式）</option>
                  </select>
                </div>

                {config.wallpaper_mode === "local" && (
                  <div className="settings-card-row">
                    <div className="settings-label">本地图库路径</div>
                    <div className="settings-description">指定包含壁纸图片的本地文件夹</div>
                    <input
                      className="settings-input"
                      type="text"
                      value={config.local_folder}
                      onChange={(e) => setConfig({ ...config, local_folder: e.target.value })}
                      placeholder="/path/to/your/images"
                    />
                  </div>
                )}

                {config.wallpaper_mode === "picsum" && (
                  <div className="settings-card-row">
                    <div className="settings-label">图片 API 地址</div>
                    <div className="settings-description">自定义 Picsum 或兼容 API 的请求地址</div>
                    <input
                      className="settings-input"
                      type="text"
                      value={config.img_api_url}
                      onChange={(e) => setConfig({ ...config, img_api_url: e.target.value })}
                      placeholder="https://picsum.photos/1920/1080"
                    />
                  </div>
                )}
              </div>

              {/* Typography & Timing */}
              <div className="settings-card">
                <div className="settings-card-row">
                  <div className="settings-label">定时更新时间</div>
                  <div className="settings-description">每日自动刷新壁纸的时间</div>
                  <input
                    className="settings-input"
                    type="time"
                    value={config.update_time}
                    onChange={(e) => setConfig({ ...config, update_time: e.target.value })}
                  />
                </div>

                <div className="settings-card-row">
                  <div className="settings-label">文字字号</div>
                  <div className="settings-description">经文叠加在壁纸上的字体大小（像素）</div>
                  <input
                    className="settings-input"
                    type="number"
                    value={config.font_size}
                    onChange={(e) => setConfig({ ...config, font_size: Number(e.target.value) })}
                    min={12}
                    max={120}
                  />
                </div>

                <div className="settings-card-row">
                  <div className="settings-label">本地字体路径</div>
                  <div className="settings-description">指定 .ttf 或 .ttc 字体文件路径</div>
                  <input
                    className="settings-input"
                    type="text"
                    value={config.font_path}
                    onChange={(e) => setConfig({ ...config, font_path: e.target.value })}
                    placeholder="/System/Library/Fonts/PingFang.ttc"
                  />
                </div>
              </div>

              {/* System */}
              <div className="settings-card">
                <div className="settings-card-row">
                  <div className="toggle-row">
                    <div>
                      <div className="settings-label">开机自动启动</div>
                      <div className="settings-description">应用随系统启动，以静默后台模式运行</div>
                    </div>
                    <label className="toggle-switch">
                      <input
                        type="checkbox"
                        checked={autoStart}
                        onChange={async () => {
                          try {
                            if (autoStart) {
                              await disable();
                              setAutoStart(false);
                              addLog("[系统] 已关闭开机自启动");
                            } else {
                              await enable();
                              setAutoStart(true);
                              addLog("[系统] 已开启开机自启动");
                            }
                          } catch (err) {
                            addLog(`[错误] 切换自启状态失败: ${err}`);
                          }
                        }}
                      />
                      <span className="toggle-track" />
                    </label>
                  </div>
                </div>
              </div>
            </div>

            {/* Tip */}
            <div className="settings-tip">
              <span className="settings-tip-icon">&#9432;</span>
              <span className="settings-tip-text">
                应用在后台运行时，您可以随时按下全局快捷键 <kbd>Cmd</kbd>+<kbd>Shift</kbd>+<kbd>W</kbd> 立即刷新桌面壁纸。
              </span>
            </div>

            {/* Save */}
            <div className="settings-save-row">
              <button className="btn btn-primary" onClick={handleSaveConfig}>
                保存配置
              </button>
            </div>
          </div>
        )}

        {/* Logs */}
        {activeTab === "logs" && (
          <div className="panel" key="logs">
            <div className="logs-title">系统日志</div>
            <div className="logs-subtitle">查看应用运行状态和操作记录。</div>

            <div className="logs-terminal">
              <div className="logs-terminal-header">
                <span className="logs-terminal-dot red" />
                <span className="logs-terminal-dot yellow" />
                <span className="logs-terminal-dot green" />
                <span className="logs-terminal-title">scripture-wallpaper</span>
              </div>
              <div className="logs-terminal-body">
                {logs.length === 0 && (
                  <div className="logs-empty">暂无日志...</div>
                )}
                {logs.map((log, i) => (
                  <div key={i} className={`log-entry ${getLogClassName(log)}`}>
                    {log}
                  </div>
                ))}
                <div ref={logsEndRef} />
              </div>
            </div>
          </div>
        )}
      </main>
    </div>
  );
}

export default App;
