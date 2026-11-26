import { Navigate, Route, Routes } from 'react-router-dom';
import { Layout } from './components/Layout';

export function App() {
  return (
    <Routes>
      <Route index element={<Layout />} />
      <Route path="*" element={<Navigate replace to="/" />} />
    </Routes>
  );
}
