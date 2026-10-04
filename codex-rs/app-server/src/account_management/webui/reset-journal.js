"use strict";

(() => {
  const { t } = window.AccountManagerMessages;
  function render(journals, api) {
    const panel = document.getElementById("reset-journals");
    panel.replaceChildren();
    panel.hidden = journals.length === 0;
    if (panel.hidden) return;
    panel.append(api.element("h2", t("Interrupted reset operations")));
    panel.append(
      api.element(
        "p",
        t(
          "Automatic credit use is paused until unresolved reset records are reviewed. Refresh quota and check reset history first.",
        ),
      ),
    );
    for (const journal of journals) {
      const row = api.element("div", "", "actions");
      row.append(
        api.element(
          "span",
          api.accounts?.find(
            (account) => account.profileId === journal.profileId,
          )?.label ||
            journal.profileId ||
            t("Unknown account"),
        ),
      );
      row.append(api.element("small", t(journal.message)));
      if (journal.archiveAvailable) {
        const archive = api.button("Archive reviewed record", () =>
          api.confirmOperation(
            "Archive interrupted reset?",
            "Only continue after independently checking this account's quota and reset history. The old request will be abandoned; a later automatic reset could use another credit. A backup is kept. This does not restore quota or spend a credit.",
            {
              type: "resetJournalArchive",
              fileName: journal.fileName,
              expectedDigest: journal.digest,
              acknowledgeUnconfirmed: true,
            },
          ),
        );
        archive.disabled = api.busy;
        row.append(archive);
      }
      panel.append(row);
    }
  }
  window.AccountManagerResetJournal = Object.freeze({ render });
})();
