import { createBrowserRouter, RouterProvider } from "react-router-dom";
import { Layout } from "./components/Layout";
import { CiCreatePage, CiEditPage } from "./pages/CiFormPage";
import { CiDetailPage } from "./pages/CiDetailPage";
import { DashboardPage } from "./pages/DashboardPage";
import { InventoryPage } from "./pages/InventoryPage";
import { NotFoundPage } from "./pages/NotFoundPage";
import { SearchPage } from "./pages/SearchPage";

const router = createBrowserRouter([
  {
    element: <Layout />,
    children: [
      { path: "/", element: <DashboardPage /> },
      { path: "/cis", element: <InventoryPage /> },
      { path: "/cis/new", element: <CiCreatePage /> },
      { path: "/cis/:id", element: <CiDetailPage /> },
      { path: "/cis/:id/edit", element: <CiEditPage /> },
      { path: "/search", element: <SearchPage /> },
      { path: "*", element: <NotFoundPage /> },
    ],
  },
]);

export function App() {
  return <RouterProvider router={router} />;
}
