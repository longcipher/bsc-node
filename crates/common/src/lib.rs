//! Common utilities and shared code.
//!
//! This crate contains shared utilities, types, and functions
//! used across multiple crates in the workspace.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors produced by cart operations.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum CartError {
    /// The cart had no items to check out.
    #[error("cannot check out an empty cart")]
    EmptyCart,
}

/// A line item in the shopping cart.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CartItem {
    /// Human-readable item name.
    pub name: String,
    /// Price per unit in cents.
    pub price_cents: u32,
    /// Selected quantity.
    pub quantity: u32,
}

/// A shopping cart.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Cart {
    /// Items currently in the cart.
    pub items: Vec<CartItem>,
}

/// An order created from checkout.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Order {
    /// Items included in the order.
    pub items: Vec<CartItem>,
    /// Total order value in cents.
    pub total_cents: u32,
}

/// The result of checking out a cart.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckoutResult {
    /// The newly created order.
    pub order: Order,
    /// The emptied cart after checkout.
    pub cart: Cart,
}

/// Builds a greeting string for the CLI.
#[must_use]
pub fn greeting(name: &str) -> String {
    format!("Hello, {name}!")
}

/// Creates an order from the provided items and clears the cart.
///
/// # Errors
///
/// Returns [`CartError::EmptyCart`] when `items` is empty, because an order
/// without any line items is not meaningful.
pub fn checkout_cart(items: &[CartItem]) -> Result<CheckoutResult, CartError> {
    if items.is_empty() {
        return Err(CartError::EmptyCart);
    }

    let order_items = items.to_vec();
    let total_cents = order_items.iter().map(|item| item.price_cents * item.quantity).sum();

    Ok(CheckoutResult { order: Order { items: order_items, total_cents }, cart: Cart::default() })
}

/// Re-exports commonly used items.
pub mod prelude {
    pub use crate::{Cart, CartError, CartItem, CheckoutResult, Order, checkout_cart, greeting};
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use crate::{CartError, CartItem, checkout_cart, greeting};

    fn cart_item_strategy() -> impl Strategy<Value = CartItem> {
        ("[A-Za-z][A-Za-z0-9 ]{0,15}", 0_u16..10_000, 0_u16..100).prop_map(
            |(name, price_cents, quantity)| CartItem {
                name,
                price_cents: u32::from(price_cents),
                quantity: u32::from(quantity),
            },
        )
    }

    #[test]
    fn greeting_builds_message() {
        assert_eq!(greeting("Rust"), "Hello, Rust!");
    }

    #[test]
    fn checkout_cart_creates_an_order_and_clears_the_cart() {
        let result = checkout_cart(&[
            CartItem { name: "Tea".to_string(), price_cents: 450, quantity: 2 },
            CartItem { name: "Cake".to_string(), price_cents: 350, quantity: 1 },
        ]);

        assert_eq!(result.as_ref().map(|r| r.order.total_cents), Ok(1250));
        assert!(matches!(result, Ok(ref r) if r.cart.items.is_empty()));
    }

    #[test]
    fn checkout_cart_empty_input_is_an_error() {
        assert_eq!(checkout_cart(&[]), Err(CartError::EmptyCart));
    }

    proptest! {
        #[test]
        fn checkout_cart_preserves_generated_items(
            items in proptest::collection::vec(cart_item_strategy(), 1..16),
        ) {
            let result = checkout_cart(&items);

            prop_assert_eq!(result.as_ref().map(|r| r.order.items.clone()), Ok(items));
            prop_assert!(matches!(result, Ok(ref r) if r.cart.items.is_empty()));
        }

        #[test]
        fn checkout_cart_total_matches_generated_line_items(
            items in proptest::collection::vec(cart_item_strategy(), 1..16),
        ) {
            let expected_total: u32 = items.iter().map(|item| item.price_cents * item.quantity).sum();
            let result = checkout_cart(&items);

            prop_assert_eq!(result.map(|r| r.order.total_cents), Ok(expected_total));
        }
    }
}
