import { createApp, h, ref } from "vue";
import ModulationWorkspace from "../../src/components/ModulationWorkspace.vue";
import { applyHostPalette } from "../../src/domain/theme-palette.mjs";
import "../../src/style.css";

const syntheticConfig = {
  source: "Depth",
  function: { kind: "sin" },
  base_speed: 1.25,
  sensitivity: 0.125,
  max_deviation: 20.5,
  phase: 0.25,
  frequency_multiplier: 1.5,
  offset: 0,
  power: 1,
  clamp_min: 10,
  clamp_max: 255,
};
const channel = {
  zones: [],
  frequency: [40, 32, 24, 16],
  intensity: [60, 50, 40, 30],
  limits: { min: 10, max: 255 },
  aggregation: "Max",
  freq_modulation: [syntheticConfig, null, null, null],
  intensity_modulation: [null, null, null, null],
};
const snapshot = {
  bridge_revision: 1,
  active_device: 0,
  active_device_mac: null,
  devices: [],
  vrchat: { connected: false, osc_port: 9000, oscquery_port: null },
  avatar_zones: [],
  config: { channel_a: channel, channel_b: channel, ukf: {}, norms: {} },
  status: {},
  alarm: { config: { enabled: false }, status: { phase: "Idle" } },
  prefs: { auto_save: false, alarm_tab_visible: false },
  remote_policy: { remote_strength: { enabled: false, max_by_channel: { A: null, B: null }, scope: "none" }, max_remote_sessions: 1 },
};

function setTheme(id) {
  applyHostPalette(id);
  document.documentElement.dataset.theme = id;
}

createApp({
  setup() {
    const theme = ref("terminal");
    const setScale = (value) => {
      document.documentElement.style.zoom = `${value}%`;
    };
    setTheme(theme.value);
    return () => h("div", { class: "app-shell" }, [
      h("main", { class: "main-area" }, [
        h("header", { class: "topbar" }, [
          h("div", { class: "breadcrumb" }, [h("span", "LOCAL FIXTURE"), h("b", "/"), h("strong", "MODULATION")]),
          h("div", { class: "topbar-right" }, [
            h("span", { class: "term-tag" }, "SYNTHETIC DATA · NO HOST CONNECTION"),
            ...["terminal", "paper", "blueprint"].map((id) => h("button", { class: "term-button", onClick: () => { theme.value = id; setTheme(id); } }, id.toUpperCase())),
            ...[100, 125, 150].map((value) => h("button", { class: "term-button", onClick: () => setScale(value) }, `${value}%`)),
          ]),
        ]),
        h("div", { class: "content-scroll" }, [
          h("section", { class: "page-heading" }, [
            h("div", [h("span", { class: "eyebrow" }, "VISUAL QA ONLY"), h("h1", "Modulation"), h("p", { class: "microcopy" }, "Synthetic channel A · frequency · segment 1")]),
          ]),
          h(ModulationWorkspace, {
            snapshot,
            writeEnabled: true,
            supports: () => true,
            onSend: (operation) => { document.body.dataset.lastOperation = JSON.stringify(operation); },
          }),
        ]),
      ]),
    ]);
  },
}).mount("#app");
