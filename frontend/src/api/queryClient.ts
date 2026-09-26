import { QueryClient } from "@tanstack/vue-query";
import { ApiError } from "./client";

/** The app's single query cache. Exported so signing in or out can drop the previous user's data. */
export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 15_000,
      refetchOnWindowFocus: false,
      // Retry only transient failures; a 4xx will not fix itself.
      retry: (count, error) => count < 2 && (!(error instanceof ApiError) || error.status === 0 || error.status >= 500),
    },
  },
});
