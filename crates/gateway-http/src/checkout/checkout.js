// The hosted payment page. Every value from the server is written with
// textContent, never as HTML, and the page only ever reads.
"use strict";

(function () {
  const token = window.location.pathname.split("/").filter(Boolean).pop() || "";
  const api = "/v1/checkout/" + encodeURIComponent(token);
  const POLL_MS = 5000;
  const SLOW_POLL_MS = 30000;
  let current = null;
  let countdown = null;

  const MESSAGES = {
    waiting: ["Waiting for your payment", "Send the exact amount below to the address shown, on the network shown."],
    confirming: ["Payment seen, waiting for confirmation", "Your transfer is on the chain. This page updates by itself once the network has finalised it; nothing else is needed from you."],
    paid: ["Paid", "The merchant has been told. You can close this page."],
    underpaid: ["Less than the full amount arrived", "The merchant will contact you about the rest. Do not send another payment unless the merchant asks you to."],
    needs_review: ["Your payment is being reviewed", "Money arrived that a person must check, for example after the quote ran out. It is not lost; the merchant will follow up."],
    expired: ["This quote has run out", "Do not send money to this quote. Ask the merchant for a new payment page."],
    cancelled: ["This order was cancelled", "Do not send money. If you already did, contact the merchant with the transaction hash."],
  };

  function field(name) {
    return Array.from(document.querySelectorAll('[data-field="' + name + '"]'));
  }

  function setText(name, value) {
    field(name).forEach(function (element) {
      element.textContent = value == null ? "" : String(value);
    });
  }

  // ISO 4217 decides how many minor digits a currency has (JPY 0, KWD 3);
  // the browser's currency data knows it, and the digits are placed as
  // text so no amount passes through a float.
  function minorDigits(currency) {
    try {
      return new Intl.NumberFormat("en", { style: "currency", currency: currency })
        .resolvedOptions().maximumFractionDigits;
    } catch (_unknownCurrency) {
      return null;
    }
  }

  function fiat(amount) {
    const minor = String(amount.minor_units);
    const digits = minorDigits(amount.currency);
    if (digits === null) {
      // Without the currency's scale a decimal point would be a guess.
      return minor + " minor units of " + amount.currency;
    }
    if (digits === 0) {
      return minor + " " + amount.currency;
    }
    const padded = minor.padStart(digits + 1, "0");
    return padded.slice(0, -digits) + "." + padded.slice(-digits) + " " + amount.currency;
  }

  function showStatus(status, title, detail) {
    const section = document.querySelector(".status");
    section.setAttribute("data-status", status);
    setText("status_title", title);
    setText("status_detail", detail);
  }

  function remaining(expiresAt) {
    const ms = new Date(expiresAt).getTime() - Date.now();
    if (ms <= 0) {
      return "expired";
    }
    const minutes = Math.floor(ms / 60000);
    const seconds = Math.floor((ms % 60000) / 1000);
    return minutes + " min " + String(seconds).padStart(2, "0") + " s";
  }

  function render(view) {
    current = view;
    setText("merchant_name", view.merchant_name);
    setText("description", view.description);
    setText("fiat", fiat(view.fiat_amount));
    setText("amount", view.amount);
    setText("symbol", view.asset.symbol);
    setText("network", view.asset.chain.toUpperCase() + " " + view.asset.network + " (" + view.asset.chain_environment + ")");
    setText("contract_address", view.asset.contract_address);
    setText("collector_address", view.collector_address);
    setText("received", view.received);

    const message = MESSAGES[view.status] || ["Unknown state", ""];
    showStatus(view.status, message[0], message[1]);

    const payable = view.status === "waiting";
    document.querySelector(".pay").hidden = !payable;
    const qr = document.querySelector(".qr");
    if (payable && !qr.getAttribute("src")) {
      qr.setAttribute("src", api + "/qr.svg");
    }

    const receipt = document.querySelector(".receipt");
    receipt.hidden = !view.transaction_hash && view.received === "0";
    field("transaction").forEach(function (link) {
      link.textContent = view.transaction_hash || "";
      if (view.explorer_url) {
        link.setAttribute("href", view.explorer_url);
      } else {
        link.removeAttribute("href");
      }
    });
  }

  function tick() {
    if (current && current.status === "waiting") {
      setText("expires_in", remaining(current.expires_at));
    }
  }

  function poll() {
    fetch(api, { headers: { Accept: "application/json" }, cache: "no-store" })
      .then(function (response) {
        if (response.status === 404) {
          showStatus("error", "Payment page not found", "Check the link the merchant gave you.");
          return null;
        }
        if (!response.ok) {
          throw new Error("HTTP " + response.status);
        }
        return response.json();
      })
      .then(function (view) {
        if (!view) {
          return;
        }
        render(view);
        const finished = view.status === "paid" || view.status === "cancelled";
        if (!finished) {
          window.setTimeout(poll, view.status === "expired" ? SLOW_POLL_MS : POLL_MS);
        }
      })
      .catch(function () {
        // An outage is said, never shown as an unpaid order; the page retries.
        showStatus("error", "Cannot reach the payment service right now", "Retrying. Your payment is not affected by this page.");
        window.setTimeout(poll, SLOW_POLL_MS);
      });
  }

  document.addEventListener("click", function (event) {
    const button = event.target.closest("button[data-copy]");
    if (!button || !current) {
      return;
    }
    const name = button.getAttribute("data-copy");
    const value = name === "contract_address" ? current.asset.contract_address : current[name];
    if (navigator.clipboard && value) {
      navigator.clipboard.writeText(String(value)).then(function () {
        button.textContent = "Copied";
        window.setTimeout(function () { button.textContent = "Copy"; }, 1500);
      });
    }
  });

  countdown = window.setInterval(tick, 1000);
  poll();
})();
