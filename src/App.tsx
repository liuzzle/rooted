import Reader from "./features/reader/Reader";
import "./App.css";

function App() {
  return (
    <div className="app">
      <div className="titlebar">
        <span className="brand">rooted</span>
        <span className="tagline">a planted tree by water</span>
      </div>
      <Reader />
    </div>
  );
}

export default App;
