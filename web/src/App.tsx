import { Navigate, Route, Routes } from 'react-router-dom';
import { Layout } from './components/Layout';
import { ProgressScreen } from './screens/ProgressScreen';
import { SummaryScreen } from './screens/SummaryScreen';
import { DocumentScreen } from './screens/DocumentScreen';
import { RequirementsScreen } from './screens/RequirementsScreen';
import { FindingsScreen } from './screens/FindingsScreen';
import { QuestionsScreen } from './screens/QuestionsScreen';
import { ExportScreen } from './screens/ExportScreen';

export function App() {
  return (
    <Routes>
      <Route element={<Layout />}>
        <Route index element={null} />
        <Route path="progress" element={<ProgressScreen />} />
        <Route path="summary" element={<SummaryScreen />} />
        <Route path="document" element={<DocumentScreen />} />
        <Route path="requirements" element={<RequirementsScreen />} />
        <Route path="findings" element={<FindingsScreen />} />
        <Route path="questions" element={<QuestionsScreen />} />
        <Route path="export" element={<ExportScreen />} />
        <Route path="*" element={<Navigate replace to="/" />} />
      </Route>
    </Routes>
  );
}
