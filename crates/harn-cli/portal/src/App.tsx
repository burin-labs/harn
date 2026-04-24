import { Navigate, Route, Routes } from "react-router-dom"

import { Sidebar } from "./components/Sidebar"
import { CostsPage } from "./pages/CostsPage"
import { DlqPage } from "./pages/DlqPage"
import { LaunchPage } from "./pages/LaunchPage"
import { RunDetailPage } from "./pages/RunDetailPage"
import { RunsPage } from "./pages/RunsPage"
import { useRunsData } from "./hooks/useRunsData"

function PortalLayout() {
  const { stats, loading, lastError, lastRefreshAt, loadRuns } = useRunsData({
    q: "",
    status: "all",
    sort: "newest",
    page: 1,
    pageSize: 1,
    poll: true,
  })

  return (
    <div className="shell">
      <Sidebar
        stats={stats}
        loading={loading}
        lastRefreshAt={lastRefreshAt}
        lastError={lastError}
        onRefresh={() => {
          void loadRuns()
        }}
      />
      <main className="main">
        <Routes>
          <Route path="/" element={<Navigate to="/launch" replace />} />
          <Route path="/launch" element={<LaunchPage />} />
          <Route path="/runs" element={<RunsPage />} />
          <Route path="/runs/detail" element={<RunDetailPage />} />
          <Route path="/dlq" element={<DlqPage />} />
          <Route path="/costs" element={<CostsPage />} />
        </Routes>
      </main>
    </div>
  )
}

export function App() {
  return <PortalLayout />
}
