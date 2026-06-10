function render() {
  var path = location.pathname;
  var label = path === "/" ? "home" : path;
  document.getElementById("route").textContent = "current route: " + label;
  document.title = "finite spa demo - " + label;
}
document.addEventListener("click", function (event) {
  var link = event.target.closest("a[data-link]");
  if (!link) return;
  event.preventDefault();
  history.pushState({}, "", link.getAttribute("href"));
  render();
});
window.addEventListener("popstate", render);
render();
