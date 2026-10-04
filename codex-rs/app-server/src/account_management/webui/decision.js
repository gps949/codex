"use strict";

(() => {
  const { t } = window.AccountManagerMessages;
  const modeLabels = {
    off: "Disabled",
    shadow: "Observe only",
    rank: "Enabled",
  };

  function defaults(provider) {
    return provider === "cloudflare"
      ? {
          model: "clef-flash",
          endpoint: "",
          api_key_env: "CLOUDFLARE_AI_TOKEN",
        }
      : {
          model: "jev-1.13.0",
          endpoint: "https://api.typesafe.ai/v1/systemone",
          api_key_env: "TYPESAFE_API_KEY",
        };
  }

  function configuration(view) {
    const saved = Object.fromEntries(
      Object.entries(view?.config || {}).filter(
        ([, value]) => value !== null && value !== undefined,
      ),
    );
    return {
      ...defaults(saved.provider || "cloudflare"),
      mode: "off",
      provider: "cloudflare",
      suggest_skills: false,
      timeout_ms: 650,
      min_confidence: 0.35,
      allow_local_http: false,
      ...saved,
    };
  }

  function accountId(endpoint) {
    return (
      String(endpoint || "").match(
        /^https:\/\/api\.cloudflare\.com\/client\/v4\/accounts\/([A-Za-z0-9_-]+)\/ai\/run\/@cf\/cloudflare\/clef(?:-flash)?$/,
      )?.[1] || ""
    );
  }

  function configure(view, api) {
    const saved = configuration(view);
    let collect;
    api.openDialog(
      t("Decision assistance"),
      "Optional help choosing tools and skills. Cloud services receive descriptions and may charge separately.",
      (body) => {
        const grid = api.element("div", "", "settings-grid");
        body.append(grid);
        const provider = api.field(
          grid,
          "decision-provider",
          "Service",
          "select",
          saved.provider,
          "Choose one service. Jev and Clef are alternatives; only the selected service is called.",
          {
            options: [
              ["cloudflare", "Cloudflare Clef"],
              ["typesafe", "TypeSafe Jev"],
            ],
          },
        );
        const mode = api.field(
          grid,
          "decision-mode",
          "Mode",
          "select",
          saved.mode,
          "Observe only sends requests but keeps the original results. Enabled applies validated suggestions.",
          {
            options: Object.entries(modeLabels).map(([value, label]) => [
              value,
              t(label),
            ]),
          },
        );
        const account = api.field(
          grid,
          "decision-account",
          "Cloudflare Account ID",
          "text",
          accountId(saved.endpoint),
          "Workers AI → Use REST API shows your Account ID and token setup.",
          { maxLength: 128, spellcheck: false, autocomplete: "off" },
        );
        const model = api.field(
          grid,
          "decision-model",
          "Model",
          "select",
          saved.model,
          "",
          {
            options: [
              ["clef-flash", "Clef-flash"],
              ["clef", "Clef"],
            ],
          },
        );
        const token = api.field(
          grid,
          "decision-token",
          "API token",
          "password",
          "",
          view?.credentialPresent
            ? "Leave blank to retain the token saved for this service. Tokens are never shown again."
            : "Paste a separate service token. No terminal environment variable is needed.",
          { maxLength: 16384, autocomplete: "new-password", spellcheck: false },
        );
        const skillHints = api.field(
          grid,
          "decision-skills",
          "Suggest skills too",
          "checkbox",
          saved.suggest_skills,
          "Adds optional skill hints; it never runs a skill or grants permissions.",
        );
        const advanced = api.element("details", "", "control-disclosure");
        advanced.append(api.element("summary", t("Advanced settings")));
        body.append(advanced);
        const customEndpoint = api.field(
          advanced,
          "decision-custom-endpoint",
          "Use a custom endpoint",
          "checkbox",
          saved.provider === "cloudflare" &&
            Boolean(saved.endpoint) &&
            !accountId(saved.endpoint),
          "For a compatible local service or your own gateway.",
        );
        const endpoint = api.field(
          advanced,
          "decision-endpoint",
          "Service endpoint",
          "url",
          saved.endpoint,
          "Clef uses your Account ID to build its endpoint automatically.",
          { maxLength: 2048, spellcheck: false },
        );
        const jevModel = api.field(
          advanced,
          "decision-jev-model",
          "Jev model",
          "text",
          saved.provider === "typesafe" ? saved.model : "jev-1.13.0",
          "",
          { maxLength: 128 },
        );
        const timeout = api.field(
          advanced,
          "decision-timeout",
          "Timeout (ms)",
          "number",
          saved.timeout_ms,
          "",
          { min: 100, max: 1500, step: 50 },
        );
        const confidence = api.field(
          advanced,
          "decision-confidence",
          "Minimum confidence",
          "number",
          saved.min_confidence,
          "",
          { min: 0, max: 1, step: 0.05 },
        );
        const environment = api.field(
          advanced,
          "decision-environment",
          "Existing token environment variable",
          "text",
          saved.api_key_env,
          "Optional compatibility with an existing host environment token.",
          { maxLength: 128, spellcheck: false },
        );
        const localHttp = api.field(
          advanced,
          "decision-local",
          "Allow HTTP on localhost",
          "checkbox",
          saved.allow_local_http,
          "Only for an existing compatible local decision service.",
        );
        function updateProvider() {
          const cloudflare = provider.value === "cloudflare";
          account.closest(".form-field").hidden =
            !cloudflare || customEndpoint.checked;
          model.closest(".form-field").hidden = !cloudflare;
          jevModel.closest(".form-field").hidden = cloudflare;
          customEndpoint.closest(".form-field").hidden = !cloudflare;
          endpoint.closest(".form-field").hidden =
            cloudflare && !customEndpoint.checked;
          account.required = cloudflare && !customEndpoint.checked;
          endpoint.required = !cloudflare || customEndpoint.checked;
          account.disabled = !cloudflare || customEndpoint.checked;
          endpoint.disabled = cloudflare && !customEndpoint.checked;
        }
        provider.addEventListener("change", () => {
          token.value = "";
          const values = defaults(provider.value);
          endpoint.value = values.endpoint;
          environment.value = values.api_key_env;
          model.value = "clef-flash";
          customEndpoint.checked = false;
          updateProvider();
        });
        customEndpoint.addEventListener("change", updateProvider);
        updateProvider();
        collect = () => {
          const cloudflare = provider.value === "cloudflare";
          return {
            config: {
              mode: mode.value,
              provider: provider.value,
              model: cloudflare ? model.value : jevModel.value.trim(),
              endpoint:
                cloudflare && !customEndpoint.checked
                  ? `https://api.cloudflare.com/client/v4/accounts/${account.value.trim()}/ai/run/@cf/cloudflare/${model.value}`
                  : endpoint.value.trim(),
              api_key_env: environment.value.trim(),
              suggest_skills: skillHints.checked,
              timeout_ms: Number(timeout.value),
              min_confidence: Number(confidence.value),
              allow_local_http: localHttp.checked,
              credential_source: saved.credential_source || "environment",
            },
            credential: token.value
              ? { type: "replace", value: token.value }
              : { type: "keep" },
            consent: mode.value !== "off",
            expectedVersion: view?.userConfigVersion || null,
          };
        };
        const actions = api.element("div", "", "actions");
        actions.append(
          api.button("Test connection", async () => {
            if (!document.getElementById("action-form").reportValidity())
              return;
            api.setBusy(true);
            try {
              const result = await api.operation({
                type: "decisionProbe",
                ...collect(),
                consent: true,
              });
              api.showResult(result.message, !result.data?.connected);
            } catch (error) {
              api.showResult(error.message, true);
            } finally {
              api.setBusy(false);
            }
          }),
        );
        if (view?.credentialSource === "saved" && view?.credentialPresent) {
          actions.append(
            api.button("Remove saved token", () => {
              token.value = "";
              api.openDialog(
                t("Remove saved token"),
                "This disables decision assistance and removes this service's saved token. Inference accounts are unchanged.",
                (details) => details.append(api.element("p", saved.endpoint)),
                () =>
                  api.perform({
                    type: "decisionSave",
                    config: {
                      ...saved,
                      mode: "off",
                      credential_source: "stored",
                    },
                    credential: { type: "remove" },
                    consent: false,
                    expectedVersion: view?.userConfigVersion || null,
                  }),
                t("Remove saved token"),
              );
            }),
          );
        }
        body.append(
          actions,
          api.element(
            "small",
            t(
              "Testing sends only built-in example text. It does not save changes and may incur a service fee.",
            ),
            "muted",
          ),
        );
        document.getElementById("action-dialog").addEventListener(
          "close",
          () => {
            token.value = "";
          },
          { once: true },
        );
      },
      async () => {
        const payload = { type: "decisionSave", ...collect() };
        document.getElementById("field-decision-token").value = "";
        return api.perform(payload);
      },
      t("Save settings"),
    );
  }

  function render(view, api) {
    const body = document.getElementById("decision-assistance");
    if (!body) return;
    body.replaceChildren();
    const config = configuration(view);
    const heading = api.element("h2", t("Decision assistance"));
    heading.id = "decision-assistance-title";
    body.setAttribute("aria-labelledby", heading.id);
    body.append(
      heading,
      api.element("p", t("Optional help choosing tools and skills."), "muted"),
      api.element(
        "p",
        `${config.provider === "cloudflare" ? "Clef" : "Jev"} · ${t(modeLabels[config.mode] || "Disabled")}`,
        "account-name",
      ),
      api.element(
        "small",
        t(view?.credentialPresent ? "Token available" : "Token not configured"),
        "muted",
      ),
    );
    if (view?.overridden)
      body.append(
        api.element(
          "small",
          t("Higher-priority settings override this saved mode."),
          "message",
        ),
      );
    if (view?.policyStatus === "blocked")
      body.append(
        api.element(
          "small",
          t(
            "Decision service destination is blocked by managed network policy.",
          ),
          "message error",
        ),
      );
    body.append(
      api.element(
        "small",
        t(
          "Applies to subsequent searches. Older running hosts need an update.",
        ),
        "muted",
      ),
    );
    const actions = api.element("div", "", "actions");
    const setup = api.button("Choose decision service", () =>
      configure(view, api),
    );
    setup.disabled = api.busy;
    actions.append(setup);
    body.append(actions);
  }

  const safeErrors = new Set([
    "Decision settings changed while this page was open. Reload before saving.",
    "Decision settings changed while this page was open. Reload before testing.",
    "Enter a token for this service before enabling decision assistance.",
    "Disable decision assistance before removing its token.",
    "Invalid decision service key",
    "Invalid decision settings; check the local configuration and service fields",
    "Decision credential storage is unavailable",
    "Higher-priority settings select another service target. Test that target with its own credentials.",
    "The independent decision key could not be stored. Settings were not saved.",
    "The independent decision key could not be removed. Settings were not saved.",
    "Decision settings could not be saved. A requested credential change may already have been stored.",
  ]);
  window.AccountManagerDecision = Object.freeze({
    render,
    safeError: (message) => (safeErrors.has(message) ? t(message) : null),
  });
})();
