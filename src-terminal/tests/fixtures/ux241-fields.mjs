import { createApp, h, ref } from "vue";
import { applyHostPalette, HOST_THEMES } from "../../src/domain/theme-palette.mjs";
import "../../src/style.css";

function setTheme(id) {
  applyHostPalette(id);
}

createApp({
  setup() {
    const activeTheme = ref("terminal");
    setTheme(activeTheme.value);
    const label = (name, control) => h("label", { class: "field-review-label", style: { display: "grid", gap: "6px" } }, [name, control]);
    const input = (type, placeholder, props = {}) => h("input", { type, placeholder, ...props });
    return () => h("main", { class: "app-shell" }, [
      h("section", { class: "panel", style: { width: "min(100%, 860px)", margin: "40px auto" } }, [
        h("div", { class: "page-heading" }, [h("h1", "Field palette review")]),
        h("p", { class: "microcopy" }, "Synthetic controls only · no Host connection"),
        h("div", { class: "button-row", style: { marginBottom: "20px" } }, HOST_THEMES.map(({ id, name }) => h("button", {
          class: "term-button",
          type: "button",
          "aria-pressed": activeTheme.value === id,
          onClick: () => { activeTheme.value = id; setTheme(id); },
        }, name.toUpperCase()))),
        h("div", { class: "field-review-grid", style: { display: "grid", gridTemplateColumns: "repeat(2,minmax(0,1fr))", gap: "14px" } }, [
          label("Text input", input("text", "Name placeholder")),
          label("Search input", input("search", "Search placeholder")),
          label("Number input", input("number", "", { value: "-255.0", step: "any" })),
          label("Select", h("select", [h("option", { value: "one" }, "One"), h("option", { value: "two" }, "Two")])),
          label("Textarea", h("textarea", { rows: 3, placeholder: "Textarea placeholder" })),
          label("Disabled", input("text", "Unavailable", { disabled: true })),
          label("Read only", h("textarea", { rows: 2, readOnly: true, value: "Read only value" })),
        ]),
      ]),
    ]);
  },
}).mount("#app");
