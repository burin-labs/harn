import type { RouteObject } from "react-router"
import { RootLayout } from "./layouts/RootLayout"
import { LandingPage } from "./pages/LandingPage"
import { DocRoute } from "./pages/DocRoute"
import { ContributePage } from "./pages/ContributePage"
import { CONTRIBUTION_PATH } from "./lib/metadata"

export const routes: RouteObject[] = [
  {
    path: "/",
    element: <RootLayout />,
    children: [
      { index: true, element: <LandingPage /> },
      { path: CONTRIBUTION_PATH, element: <ContributePage /> },
      { path: "*", element: <DocRoute /> },
    ],
  },
]
