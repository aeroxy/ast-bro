def calls(list)
  Shop::Pricing.price(list)
  ::File.join("a", "b")
  Cart.total(list)
  Cart::build.run
  ENV.fetch("HOME")
  list.size
end
