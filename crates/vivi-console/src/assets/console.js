// vivi console: optional auto-refresh. Off by default. When the box is
// ticked, fetch this page every 10 s and swap only the main region.
(function () {
  var box = document.getElementById("auto-refresh");
  if (!box) return;
  var timer = null;
  function refresh() {
    fetch(location.href, { credentials: "same-origin", cache: "no-store" })
      .then(function (r) { return r.ok ? r.text() : null; })
      .then(function (text) {
        if (!text) return;
        var next = new DOMParser().parseFromString(text, "text/html").getElementById("main");
        var cur = document.getElementById("main");
        if (next && cur) cur.innerHTML = next.innerHTML;
      })
      .catch(function () {});
  }
  box.addEventListener("change", function () {
    clearInterval(timer);
    timer = box.checked ? setInterval(refresh, 10000) : null;
  });
})();
