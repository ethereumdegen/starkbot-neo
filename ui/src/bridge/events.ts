// The one subscription.
//
// 04 §15: a single `listen()` feeds the store and every component selects
// from it. Two listeners would mean two orderings of the same stream and two
// `lastSeq` counters, and the one that noticed a gap would repair state the
// other had already rewritten.

import { useEffect } from "react";

import { onAppEvent } from "./api";
import { useStore } from "../store/store";

/**
 * Subscribe first, bootstrap second.
 *
 * The other order has a hole in it: an event published between the bootstrap
 * read and the subscription is lost, and since it never arrives there is no
 * `seq` jump to notice it by. Subscribing first can only duplicate — and a
 * duplicate is harmless, since the bootstrap replaces state rather than
 * appending to it.
 */
export function useAppEvents(): void {
  useEffect(() => {
    let live = true;
    const subscription = onAppEvent((envelope) => {
      if (live) {
        useStore.getState().applyEvent(envelope);
      }
    });
    void subscription.then(() => {
      if (live) {
        void useStore.getState().bootstrap();
      }
    });
    return () => {
      live = false;
      void subscription.then((off) => off());
    };
  }, []);
}
