type TodayRouteLoading = {
  goalsLoading: boolean;
};

/**
 * Only the route's own narrative sources gate the page shell.
 *
 * Cost and fleet feed supporting tiles; holding the whole Today page behind them turned one slow
 * secondary query into a blank route.
 */
export function todayRouteLoading({ goalsLoading }: TodayRouteLoading): boolean {
  return goalsLoading;
}
