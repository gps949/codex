"use strict";

(() => {
  const maximum = 64;
  const text = (value, limit) =>
    typeof value === "string" && value.length > 0 && value.length <= limit;
  const verified = (value) =>
    Boolean(
      value &&
        typeof value.ownerKey === "string" &&
        /^[a-f0-9]{64}$/.test(value.ownerKey) &&
        text(value.creditId, 256) &&
        text(value.idempotencyKey, 128),
    );
  const stored = (value) =>
    Boolean(
      value &&
        text(value.profileId, 256) &&
        text(value.creditId, 256) &&
        text(value.idempotencyKey, 128) &&
        (value.ownerKey === undefined || verified(value)),
    );
  const same = (left, right) =>
    left.idempotencyKey === right.idempotencyKey &&
    left.ownerKey === right.ownerKey &&
    left.creditId === right.creditId;
  function create(storage, key) {
    let records = [];
    let problem = false;
    try {
      const saved = JSON.parse(storage.getItem(key) || "null");
      records =
        saved === null ? [] : saved.version === 2 ? saved.operations : [saved];
      if (
        !Array.isArray(records) ||
        records.length > maximum ||
        records.some((record) => !stored(record)) ||
        new Set(records.map((record) => record.idempotencyKey)).size !==
          records.length
      )
        throw new Error("Invalid reset retry collection");
      records = records.map((record) => Object.freeze({ ...record }));
    } catch {
      records = [];
      problem = true;
    }
    function persist(next) {
      if (problem)
        throw new Error(
          "Stored reset operations could not be verified. Review the original records before starting another reset.",
        );
      try {
        if (next.length)
          storage.setItem(
            key,
            JSON.stringify({ version: 2, operations: next }),
          );
        else storage.removeItem(key);
      } catch {
        throw new Error(
          "Browser session storage is unavailable. Enable it before consuming a credit so an uncertain operation can be retried safely.",
        );
      }
      records = next;
    }
    function retain(value, origin) {
      if (!stored(value) || !verified(value))
        throw new Error(
          "The pending reset could not be verified. Reload credits before retrying.",
        );
      const existing = records.find(
        (record) =>
          record.idempotencyKey === value.idempotencyKey ||
          (origin === "browser" && record.ownerKey === value.ownerKey) ||
          (!record.ownerKey && record.profileId === value.profileId),
      );
      if (
        existing &&
        !(
          same(existing, value) ||
          (!existing.ownerKey &&
            existing.profileId === value.profileId &&
            existing.idempotencyKey === value.idempotencyKey &&
            existing.creditId === value.creditId)
        )
      )
        throw new Error(
          "An original reset operation is already retained for this account. Review it before replacing its retry record.",
        );
      if (!existing && records.length >= maximum)
        throw new Error(
          "The browser retains 64 reset operations. Review an existing operation before starting another reset.",
        );
      const next = Object.freeze({ ...value });
      persist(
        existing
          ? records.map((record) => (record === existing ? next : record))
          : [...records, next],
      );
      return next;
    }
    return Object.freeze({
      get problem() {
        return problem;
      },
      all: () => [...records],
      current: (profile, owner, operation) =>
        records.find(
          (record) =>
            verified(record) &&
            record.ownerKey === owner &&
            (operation === undefined || record.idempotencyKey === operation),
        ) ||
        records.find(
          (record) => record.profileId === profile && !record.ownerKey,
        ),
      put: (value) => retain(value, "browser"),
      recover: (value) => retain(value, "server"),
      remove: (value) => {
        if (!records.some((record) => same(record, value)))
          throw new Error(
            "The pending reset operation changed. Review its current record again.",
          );
        persist(records.filter((record) => !same(record, value)));
      },
    });
  }
  window.AccountManagerResetOperations = Object.freeze({ create, verified });
})();
