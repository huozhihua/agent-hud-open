import { useState, type CSSProperties } from "react";
import { ChevronDown, ChevronUp, RefreshCw } from "lucide-react";
import type {
  NativeMethod,
  PanelLayout,
  PanelBlockLayout,
  Snapshot,
  SourcePreferences,
} from "@agent-hud/protocol";
import { Button } from "@agent-hud/ui/components/button";
import { Switch } from "@agent-hud/ui/components/switch";
import { Input } from "@agent-hud/ui/components/input";
import CodexAccountHomes from "./settings/CodexAccountHomes";
import AntigravityAccounts from "./settings/AntigravityAccounts";
import { HUDArtwork as HUDLogo } from "@agent-hud/hud/HUDArtwork";
import {
  vendorKey,
  inRowOrder,
  quotaAccountKey,
  quotaRowHidden,
  quotaRowKey,
  quotaRowMatches,
  withRowShown,
} from "@agent-hud/hud/model";
import { snapshotLanguage, quotaPercentLabel } from "@agent-hud/ui/format";
interface Props {
  snapshot: Snapshot;
  action: (
    method: NativeMethod,
    params?: Record<string, unknown>,
  ) => Promise<unknown>;
  layout: PanelLayout;
  onLayout: (layout: PanelLayout) => Promise<unknown>;
  preferences: Record<string, SourcePreferences>;
  onPreferences: (source: string, patch: SourcePreferences) => Promise<unknown>;
}
const box: CSSProperties = {
  border: "1px solid var(--border)",
  background: "var(--card)",
  borderRadius: 12,
  overflow: "hidden",
};
const row: CSSProperties = {
  display: "flex",
  alignItems: "center",
  gap: 12,
  padding: "12px 16px",
};
export default function SourcesSettings({
  snapshot,
  action,
  layout,
  onLayout,
  preferences,
  onPreferences,
}: Props) {
  const [selectedID, setSelected] = useState<string>();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [credentials, setCredentials] = useState<Record<string, string>>({});
  const [adding, setAdding] = useState(false);
  const zh = snapshotLanguage(snapshot) === "zh",
    t = (en: string, cn: string) => (zh ? cn : en);
  const providerOrder = (layout.order ?? []).map((key) =>
    key.replace(/^api:/, ""),
  );
  const providers = [...snapshot.providers].sort(
    (a, b) =>
      (providerOrder.includes(a.id) ? providerOrder.indexOf(a.id) : Infinity) -
        (providerOrder.includes(b.id)
          ? providerOrder.indexOf(b.id)
          : Infinity) || Number(b.detected) - Number(a.detected),
  );
  const source = providers.find((p) => p.id === selectedID) ?? providers[0];
  const windows = snapshot.quotas.filter(
    (q) => source && vendorKey(q.vendor) === vendorKey(source.id),
  );
  const billing = snapshot.billing.filter(
    (account) => source && vendorKey(account.vendor) === vendorKey(source.id),
  );
  const key =
    windows[0]?.vendor ??
    (billing[0] ? `api:${billing[0].vendor}` : (source?.id ?? ""));
  const block = layout.blocks?.[key] ?? {};
  const profiles = (snapshot.accounts ?? []).filter(
    (account) => source && vendorKey(account.provider) === vendorKey(source.id),
  );
  const accounts = [...new Set(windows.map(quotaAccountKey))];
  // The relays this client is configured with, and the ones agreed to.
  const relays = (snapshot.relays ?? []).filter(
    (relay) => source && relay.client === source.id,
  );
  const consented = snapshot.settings.relayReading ?? [];
  // The clients whose relays can be added by hand.
  const relayClients = ["claude", "codex"];
  const relayKinds: Record<string, string> = {
    newapi: "New API",
    sub2api: "Sub2API",
    openrouter: "OpenRouter",
  };
  const fields = (
    (source?.accountStatus as unknown as { configurableFields?: string[] })
      ?.configurableFields ?? []
  ).map((key) => ({
    key,
    label:
      (
        {
          apiKey: "API key",
          baseUrl: "API endpoint",
          organization: "Organization",
          project: "Project",
        } as Record<string, string>
      )[key] ?? key,
    secret: key === "apiKey",
    required: key === "apiKey",
  }));
  async function perform(job: () => Promise<unknown>) {
    setPending(true);
    setError(null);
    try {
      await job();
      return true;
    } catch (error) {
      setError(String(error));
      return false;
    } finally {
      setPending(false);
    }
  }
  const changeBlock = (patch: Partial<PanelBlockLayout>) =>
    perform(() =>
      onLayout({
        ...layout,
        blocks: { ...layout.blocks, [key]: { ...block, ...patch } },
      }),
    );
  const move = (values: string[], id: string, direction: number) => {
    const at = values.indexOf(id),
      to = at + direction;
    if (at < 0 || to < 0 || to >= values.length) return values;
    const next = [...values];
    [next[at], next[to]] = [next[to], next[at]];
    return next;
  };
  const availableKeys = [
    ...new Set([
      ...snapshot.quotas.map((q) => q.vendor),
      ...snapshot.billing.map((account) => `api:${account.vendor}`),
    ]),
  ];
  const order = [
    ...(layout.order ?? []),
    ...availableKeys.filter((id) => !(layout.order ?? []).includes(id)),
  ];
  // Each row once, by the key a layout writes for it, wherever the layout's
  // order names it by any of its keys.
  const windowOrder = [
    ...new Set(inRowOrder(windows, block.rows ?? []).map(quotaRowKey)),
  ];
  return (
    <div
      className="source-settings"
      style={{
        display: "flex",
        gap: 20,
        alignItems: "flex-start",
        width: "100%",
      }}
    >
      <aside style={{ width: 185, flexShrink: 0, display: "grid", gap: 3 }}>
        {providers.map((provider) => (
          <button
            key={provider.id}
            onClick={() => {
              setSelected(provider.id);
              setCredentials({});
              setError(null);
            }}
            style={{
              display: "flex",
              gap: 10,
              alignItems: "center",
              textAlign: "left",
              padding: "10px 12px",
              border: 0,
              borderRadius: 8,
              color: "inherit",
              background:
                provider.id === source?.id ? "var(--accent)" : "transparent",
            }}
          >
            <HUDLogo source={provider.id} size={19} />
            <span style={{ flex: 1 }}>{provider.name}</span>
            <span
              aria-label={
                provider.detected
                  ? t("Detected", "已检测")
                  : t("Not detected", "未检测")
              }
              style={{
                width: 5,
                height: 5,
                borderRadius: 9,
                background: provider.detected
                  ? "#39c88a"
                  : "var(--muted-foreground)",
              }}
            />
          </button>
        ))}
      </aside>
      {source && (
        <div style={{ flex: 1, minWidth: 0, display: "grid", gap: 22 }}>
          <header style={{ display: "flex", alignItems: "center", gap: 10 }}>
            <HUDLogo source={source.id} size={30} />
            <h2 style={{ fontSize: 20, margin: 0, flex: 1 }}>{source.name}</h2>
            <Button
              variant="ghost"
              size="icon-sm"
              disabled={pending}
              onClick={() => void perform(() => action("refresh"))}
            >
              <RefreshCw size={15} />
            </Button>
          </header>
          <section style={box}>
            <div style={row}>
              <div style={{ flex: 1 }}>
                <strong>{t("Enabled", "启用")}</strong>
                <p
                  style={{
                    fontSize: 12,
                    color: "var(--muted-foreground)",
                    margin: "4px 0 0",
                  }}
                >
                  {source.notice}
                </p>
              </div>
              <Switch
                checked={source.enabled}
                disabled={pending}
                onCheckedChange={(enabled) =>
                  void perform(() =>
                    action("set_provider", { id: source.id, enabled }),
                  )
                }
              />
            </div>
            <div style={{ ...row, borderTop: "1px solid var(--border)" }}>
              <span style={{ flex: 1 }}>{t("Live status", "实时状态")}</span>
              <Switch
                checked={preferences[source.id]?.live !== false}
                disabled={pending}
                onCheckedChange={(live) =>
                  void perform(() => onPreferences(source.id, { live }))
                }
              />
            </div>
            <div style={{ ...row, borderTop: "1px solid var(--border)" }}>
              <span style={{ flex: 1 }}>
                {t("Completion reminders", "完成提醒")}
              </span>
              <Switch
                checked={preferences[source.id]?.reminders !== false}
                disabled={pending}
                onCheckedChange={(reminders) =>
                  void perform(() => onPreferences(source.id, { reminders }))
                }
              />
            </div>
          </section>
          {source.id === "codex" && (
            <CodexAccountHomes
              homes={snapshot.settings.codexHomes ?? []}
              zh={zh}
              onSave={(homes) =>
                action("set_setting", { key: "codexHomes", value: homes })
              }
            />
          )}
          {source.id === "antigravity" && (
            <AntigravityAccounts
              settings={snapshot.settings.antigravityCliSettings ?? []}
              zh={zh}
              action={action}
            />
          )}
          <section>
            <h3 style={{ fontSize: 14 }}>
              {t("Expanded panel on this display", "此屏幕的展开面板")}
            </h3>
            <div style={box}>
              <div style={row}>
                <span style={{ flex: 1 }}>
                  {t("Show this block", "显示此分组")}
                </span>
                <Switch
                  checked={!block.hidden}
                  disabled={pending}
                  onCheckedChange={(shown) =>
                    void changeBlock({ hidden: !shown })
                  }
                />
                <Button
                  size="icon-xs"
                  variant="ghost"
                  disabled={pending || order.indexOf(key) <= 0}
                  onClick={() =>
                    void perform(() =>
                      onLayout({ ...layout, order: move(order, key, -1) }),
                    )
                  }
                >
                  <ChevronUp size={13} />
                </Button>
                <Button
                  size="icon-xs"
                  variant="ghost"
                  disabled={
                    pending ||
                    order.indexOf(key) < 0 ||
                    order.indexOf(key) === order.length - 1
                  }
                  onClick={() =>
                    void perform(() =>
                      onLayout({ ...layout, order: move(order, key, 1) }),
                    )
                  }
                >
                  <ChevronDown size={13} />
                </Button>
              </div>
              {accounts.map((account) => {
                const checked =
                  !block.hidden &&
                  !(block.hiddenAccounts ?? []).includes(account) &&
                  (!block.accounts?.length || block.accounts.includes(account));
                const profile = profiles.find(
                  (profile) => profile.id === account,
                );
                const accountWindows = windows.filter(
                  (q) => quotaAccountKey(q) === account,
                );
                return (
                  <div
                    key={account || "unknown"}
                    style={{ borderTop: "1px solid var(--border)" }}
                  >
                    <div style={row}>
                      <div style={{ flex: 1, minWidth: 0 }}>
                        <strong style={{ fontSize: 13 }}>
                          {profile?.name ??
                            accountWindows[0]?.account ??
                            profile?.label ??
                            t("Account unavailable", "账户未知")}
                        </strong>
                        <p
                          style={{
                            margin: "3px 0 0",
                            fontSize: 12,
                            color: "var(--muted-foreground)",
                          }}
                        >
                          {profile?.planLabel ??
                            profile?.plan ??
                            t("Plan unavailable", "套餐未知")}
                        </p>
                      </div>
                      <Switch
                        checked={checked}
                        disabled={pending}
                        aria-label={t("Show account", "显示账户")}
                        onCheckedChange={(show) => {
                          let selected = block.hidden
                            ? []
                            : block.accounts?.length
                              ? [...block.accounts]
                              : [...accounts];
                          selected = show
                            ? [...new Set([...selected, account])]
                            : selected.filter((id) => id !== account);
                          void changeBlock({
                            accounts: block.accounts?.length
                              ? selected
                              : undefined,
                            hiddenAccounts: show
                              ? (block.hiddenAccounts ?? []).filter(
                                  (id) => id !== account,
                                )
                              : [
                                  ...new Set([
                                    ...(block.hiddenAccounts ?? []),
                                    account,
                                  ]),
                                ],
                            hidden: false,
                          });
                        }}
                      />
                    </div>
                    {windowOrder
                      .map((id) =>
                        accountWindows.find((q) => quotaRowMatches(q, id)),
                      )
                      .filter((q): q is NonNullable<typeof q> => Boolean(q))
                      .map((window) => (
                        <div
                          key={quotaRowKey(window)}
                          draggable={!pending}
                          onDragStart={(event) =>
                            event.dataTransfer.setData(
                              "text/agent-hud-window",
                              quotaRowKey(window),
                            )
                          }
                          onDragOver={(event) => event.preventDefault()}
                          onDrop={(event) => {
                            event.preventDefault();
                            const from = event.dataTransfer.getData(
                              "text/agent-hud-window",
                            );
                            if (!windowOrder.includes(from)) return;
                            const values = windowOrder.filter(
                              (id) => id !== from,
                            );
                            values.splice(
                              values.indexOf(quotaRowKey(window)),
                              0,
                              from,
                            );
                            void changeBlock({ rows: values });
                          }}
                          style={{
                            ...row,
                            padding: "9px 16px 9px 30px",
                            fontSize: 12,
                            cursor: "grab",
                          }}
                        >
                          <span style={{ flex: 1 }}>{window.name}</span>
                          <span style={{ color: "var(--muted-foreground)" }}>
                            {quotaPercentLabel(
                              window.usedPercent,
                              snapshot.settings.quotaDisplay,
                            )}
                          </span>
                          <Switch
                            checked={
                              !quotaRowHidden(window, block.hiddenRows ?? [])
                            }
                            disabled={pending}
                            aria-label={t("Show quota window", "显示额度窗口")}
                            onCheckedChange={(show) =>
                              void changeBlock({
                                hiddenRows: show
                                  ? withRowShown(
                                      block.hiddenRows ?? [],
                                      window,
                                      windows,
                                    )
                                  : [
                                      ...new Set([
                                        ...(block.hiddenRows ?? []),
                                        quotaRowKey(window),
                                      ]),
                                    ],
                              })
                            }
                          />
                          <Button
                            size="icon-xs"
                            variant="ghost"
                            disabled={
                              pending ||
                              windowOrder.indexOf(quotaRowKey(window)) === 0
                            }
                            onClick={() =>
                              void changeBlock({
                                rows: move(
                                  windowOrder,
                                  quotaRowKey(window),
                                  -1,
                                ),
                              })
                            }
                          >
                            <ChevronUp size={12} />
                          </Button>
                          <Button
                            size="icon-xs"
                            variant="ghost"
                            disabled={
                              pending ||
                              windowOrder.indexOf(quotaRowKey(window)) ===
                                windowOrder.length - 1
                            }
                            onClick={() =>
                              void changeBlock({
                                rows: move(windowOrder, quotaRowKey(window), 1),
                              })
                            }
                          >
                            <ChevronDown size={12} />
                          </Button>
                        </div>
                      ))}
                  </div>
                );
              })}
              {!windows.length && (
                <p
                  style={{
                    ...row,
                    fontSize: 12,
                    color: "var(--muted-foreground)",
                  }}
                >
                  {t(
                    "No quota windows are currently available. Recorded usage and live sessions remain independent.",
                    "当前没有可用额度窗口；用量记录与实时会话独立显示。",
                  )}
                </p>
              )}
            </div>
          </section>
          {source.id === "copilot" && (
            <section style={box}>
              <div style={row}>
                <div style={{ flex: 1 }}>
                  <strong>
                    {t("Read GitHub Copilot quota", "读取 GitHub Copilot 额度")}
                  </strong>
                  <p
                    style={{
                      fontSize: 12,
                      color: "var(--muted-foreground)",
                      margin: "4px 0 0",
                    }}
                  >
                    {t(
                      "Use this device's signed-in GitHub credentials to query Copilot quota.",
                      "使用此设备上已登录的 GitHub 凭据查询 Copilot 额度。",
                    )}
                  </p>
                </div>
                <Switch
                  checked={snapshot.settings.readCopilotQuota === true}
                  disabled={pending}
                  onCheckedChange={(value) =>
                    void perform(() =>
                      action("set_setting", { key: "readCopilotQuota", value }),
                    )
                  }
                />
              </div>
            </section>
          )}
          {(relays.length > 0 || relayClients.includes(source.id)) && (
            <section>
              <h3 style={{ fontSize: 14 }}>{t("Relays", "中转站")}</h3>
              <p
                style={{
                  fontSize: 12,
                  color: "var(--muted-foreground)",
                  margin: "0 0 8px",
                }}
              >
                {t(
                  `Read a relay's balance with the key ${source.name} already uses for it. The key goes only to that address.`,
                  `用 ${source.name} 已经为中转站配置的 key 读取它的余额。key 只发给这个地址。`,
                )}
              </p>
              <div style={{ ...box, display: "grid", gap: 10 }}>
                {relays.map((relay) => {
                  const agreed = consented.includes(relay.provider);
                  const kind = relay.kind ? relayKinds[relay.kind] : undefined;
                  return (
                    <div key={relay.id ?? relay.provider} style={row}>
                      <div style={{ flex: 1, minWidth: 0 }}>
                        <strong style={{ fontSize: 13 }}>
                          {relay.name ?? relay.provider}
                        </strong>
                        <p
                          style={{
                            fontSize: 12,
                            color: "var(--muted-foreground)",
                            margin: "2px 0 0",
                          }}
                        >
                          {[
                            relay.manual
                              ? `${relay.provider} · ${t("added by hand", "手动添加")}`
                              : undefined,
                            kind,
                            relay.notice,
                          ]
                            .filter(Boolean)
                            .join(" · ") ||
                            t("Found in its configuration", "从配置中发现")}
                        </p>
                      </div>
                      {relay.manual ? (
                        <Button
                          variant="outline"
                          size="sm"
                          disabled={pending}
                          onClick={() =>
                            void perform(() =>
                              action("remove_relay_source", { id: relay.id }),
                            )
                          }
                        >
                          {t("Remove", "移除")}
                        </Button>
                      ) : (
                        <Switch
                          checked={agreed}
                          disabled={pending}
                          aria-label={t(
                            `Read ${relay.provider}'s balance`,
                            `读取 ${relay.provider} 的余额`,
                          )}
                          onCheckedChange={(value) =>
                            void perform(() =>
                              action("set_setting", {
                                key: "relayReading",
                                value: value
                                  ? [...consented, relay.provider]
                                  : consented.filter(
                                      (host) => host !== relay.provider,
                                    ),
                              }),
                            )
                          }
                        />
                      )}
                    </div>
                  );
                })}
                {relays.length === 0 && (
                  <p
                    style={{
                      ...row,
                      fontSize: 12,
                      color: "var(--muted-foreground)",
                    }}
                  >
                    {t(
                      `No relay was found in ${source.name}'s configuration.`,
                      `${source.name} 的配置里没有找到中转站。`,
                    )}
                  </p>
                )}
                {relayClients.includes(source.id) &&
                  (adding ? (
                    <RelayForm
                      t={t}
                      pending={pending}
                      onCancel={() => setAdding(false)}
                      onSave={(relay) =>
                        void perform(() =>
                          action("add_relay_source", {
                            ...relay,
                            client: source.id,
                          }),
                        ).then((saved) => {
                          if (saved) setAdding(false);
                        })
                      }
                    />
                  ) : (
                    <div style={row}>
                      <Button
                        variant="outline"
                        size="sm"
                        disabled={pending}
                        onClick={() => setAdding(true)}
                      >
                        {t("Add a relay", "添加中转站")}
                      </Button>
                    </div>
                  ))}
              </div>
            </section>
          )}
          {fields.length > 0 && (
            <section>
              <h3 style={{ fontSize: 14 }}>{t("Connection", "连接配置")}</h3>
              <form
                style={{ ...box, padding: 16, display: "grid", gap: 12 }}
                onSubmit={(event) => {
                  event.preventDefault();
                  void perform(() =>
                    action("set_credentials", {
                      provider: source.id,
                      credentials,
                    }),
                  ).then((saved) => {
                    if (saved) setCredentials({});
                  });
                }}
              >
                {fields.map((field) => (
                  <label
                    key={field.key}
                    style={{ display: "grid", gap: 5, fontSize: 12 }}
                  >
                    {field.label}
                    <Input
                      type={field.secret ? "password" : "text"}
                      autoComplete="off"
                      required={field.required}
                      value={credentials[field.key] ?? ""}
                      onChange={(event) =>
                        setCredentials((previous) => ({
                          ...previous,
                          [field.key]: event.target.value,
                        }))
                      }
                    />
                  </label>
                ))}
                <div
                  style={{
                    display: "flex",
                    gap: 8,
                    justifyContent: "flex-end",
                  }}
                >
                  <Button
                    type="button"
                    variant="outline"
                    disabled={pending}
                    onClick={() =>
                      void perform(() =>
                        action("remove_credentials", { provider: source.id }),
                      )
                    }
                  >
                    {t("Remove", "移除")}
                  </Button>
                  <Button disabled={pending} type="submit">
                    {t("Save", "保存")}
                  </Button>
                </div>
              </form>
            </section>
          )}
          {error && (
            <p
              role="alert"
              style={{ color: "var(--destructive)", fontSize: 12 }}
            >
              {error}
            </p>
          )}
        </div>
      )}
    </div>
  );
}

/** A relay added by hand: its name, address, key and how its balance is read. */
function RelayForm({
  t,
  pending,
  onSave,
  onCancel,
}: {
  t: (en: string, zh: string) => string;
  pending: boolean;
  onSave: (relay: Record<string, unknown>) => void;
  onCancel: () => void;
}) {
  const [name, setName] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [key, setKey] = useState("");
  const [kind, setKind] = useState("auto");
  const [url, setUrl] = useState("");
  const [remaining, setRemaining] = useState("");
  const [divisor, setDivisor] = useState("1");
  const [currency, setCurrency] = useState("USD");
  const [header, setHeader] = useState("");
  const kinds: [string, string][] = [
    ["auto", t("Find out", "自动识别")],
    ["newapi", "New API"],
    ["sub2api", "Sub2API"],
    ["openrouter", "OpenRouter"],
    ["custom", t("Custom", "自定义")],
  ];
  const field = (
    label: string,
    value: string,
    change: (value: string) => void,
    options: { secret?: boolean; placeholder?: string } = {},
  ) => (
    <label style={{ display: "grid", gap: 4, fontSize: 12 }}>
      {label}
      <Input
        type={options.secret ? "password" : "text"}
        autoComplete="off"
        value={value}
        placeholder={options.placeholder}
        onChange={(event) => change(event.target.value)}
      />
    </label>
  );
  return (
    <form
      style={{ display: "grid", gap: 10, padding: 16 }}
      onSubmit={(event) => {
        event.preventDefault();
        onSave({
          name,
          baseUrl,
          key,
          kind,
          ...(kind === "custom"
            ? {
                custom: {
                  url,
                  remaining,
                  divisor: Number(divisor) || 1,
                  currency,
                  header,
                },
              }
            : {}),
        });
      }}
    >
      {field(t("Name", "名称"), name, setName)}
      {field(t("Address", "地址"), baseUrl, setBaseUrl, {
        placeholder: "https://relay.example.com",
      })}
      {field(t("Key", "Key"), key, setKey, { secret: true })}
      <label style={{ display: "grid", gap: 4, fontSize: 12 }}>
        {t("Type", "类型")}
        <select
          value={kind}
          onChange={(event) => setKind(event.target.value)}
          style={{ height: 30, borderRadius: 6, fontSize: 12 }}
        >
          {kinds.map(([value, label]) => (
            <option key={value} value={value}>
              {label}
            </option>
          ))}
        </select>
      </label>
      {kind === "custom" && (
        <>
          {field(t("Balance endpoint", "余额接口"), url, setUrl, {
            placeholder: t(
              "/api/balance or https://…",
              "/api/balance 或 https://…",
            ),
          })}
          {field(
            t("Balance in the answer", "余额在返回里的位置"),
            remaining,
            setRemaining,
            {
              placeholder: "data.balance",
            },
          )}
          {field(t("Divide by", "除数"), divisor, setDivisor)}
          <label style={{ display: "grid", gap: 4, fontSize: 12 }}>
            {t("Currency", "币种")}
            <select
              value={currency}
              onChange={(event) => setCurrency(event.target.value)}
              style={{ height: 30, borderRadius: 6, fontSize: 12 }}
            >
              <option value="USD">USD</option>
              <option value="CNY">CNY</option>
            </select>
          </label>
          {field(
            t("Key header (optional)", "key 放在哪个请求头（可不填）"),
            header,
            setHeader,
            {
              placeholder: "Authorization",
            },
          )}
        </>
      )}
      <p style={{ fontSize: 11, color: "var(--muted-foreground)", margin: 0 }}>
        {t(
          "The key is kept privately on this Mac and sent only to this address.",
          "key 只保存在这台 Mac 上，只发给这个地址。",
        )}
      </p>
      <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          disabled={pending}
          onClick={onCancel}
        >
          {t("Cancel", "取消")}
        </Button>
        <Button
          type="submit"
          size="sm"
          disabled={pending || !name.trim() || !baseUrl.trim() || !key.trim()}
        >
          {t("Add", "添加")}
        </Button>
      </div>
    </form>
  );
}
