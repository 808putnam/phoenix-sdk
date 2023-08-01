/**
 * This code was GENERATED using the solita package.
 * Please DO NOT EDIT THIS FILE, instead rerun solita to update it or write a wrapper to add functionality.
 *
 * See: https://github.com/metaplex-foundation/solita
 */

import * as beet from "@metaplex-foundation/beet";
import { CondensedOrder, condensedOrderBeet } from "./CondensedOrder";
import {
  FailedMultipleLimitOrderBehavior,
  failedMultipleLimitOrderBehaviorBeet,
} from "./FailedMultipleLimitOrderBehavior";
export type MultipleOrderPacket = {
  bids: CondensedOrder[];
  asks: CondensedOrder[];
  clientOrderId: beet.COption<beet.bignum>;
  failedMultipleLimitOrderBehavior: FailedMultipleLimitOrderBehavior;
};

/**
 * @category userTypes
 * @category generated
 */
export const multipleOrderPacketBeet =
  new beet.FixableBeetArgsStruct<MultipleOrderPacket>(
    [
      ["bids", beet.array(condensedOrderBeet)],
      ["asks", beet.array(condensedOrderBeet)],
      ["clientOrderId", beet.coption(beet.u128)],
      [
        "failedMultipleLimitOrderBehavior",
        failedMultipleLimitOrderBehaviorBeet,
      ],
    ],
    "MultipleOrderPacket"
  );
