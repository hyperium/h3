use bytes::Buf;

pub struct Framed<B> {
    _frame: Frame<B>,
}

#[allow(unused)]
enum Frame<B> {
    Headers,
    Data(B),
}

impl<B: Buf> Buf for Framed<B> {
    fn remaining(&self) -> usize { todo!() }
    fn bytes(&self) -> &[u8] { todo!() }
    fn advance(&mut self, _: usize) { todo!() }
}
